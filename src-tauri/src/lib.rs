//! 无界启动器 (BoundLaunch) - 后端入口
//!
//! 模块组织详见 `PR/03-模块设计/00-模块总览.md`

// 启动器目前阶段（Phase 0-2）只接入了少量 commands，多数 Service/Helper 的 API
// 是为后续阶段预留的（core_checkout / plugin_install / process_kill_stale 等）。
// 全局屏蔽 dead_code，避免 CI 噪音；后续逐步接入时再移除该属性。
//
// 同时屏蔽 unused_imports：模块 `pub use models::{...}` 的 re-export 是为
// `#[cfg(test)]` 模块使用的，主 lib 编译时这些 import 看起来未用。这与 dead_code
// 同属"为后续阶段预留"的同一类问题，一并屏蔽避免 CI 噪音。
#![allow(dead_code)]
#![allow(unused_imports)]

use tracing_subscriber::EnvFilter;
use tauri::Manager;

mod app_state;
mod commands;
mod common;
mod config;
mod core_manager;
mod env_inspector;
mod error;
mod event_bus;
mod log_store;
mod model_path;
mod plugin_manager;
mod process_launcher;
mod python_env;
mod system;
mod task_scheduler;
mod tray;
mod uv_sidecar;

use crate::common::paths;
use crate::config::ConfigService;
use crate::core_manager::CoreManagerService;
use crate::env_inspector::EnvironmentInspectorService;
use crate::log_store::LogStoreService;
use crate::model_path::ModelPathService;
use crate::plugin_manager::PluginManagerService;
use crate::process_launcher::ProcessLauncherService;
use crate::process_launcher::ShutdownCoordinator;
use crate::python_env::PythonEnvService;
use crate::python_env::TransformersVersionIndex;
use crate::task_scheduler::TaskSchedulerService;

pub fn run() {
    // 日志初始化
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_target(false)
        .init();

    tracing::info!("BoundLaunch launcher starting up...");

    tauri::Builder::default()
    .plugin(tauri_plugin_shell::init())
    .plugin(tauri_plugin_dialog::init())
    .plugin(tauri_plugin_process::init())
        .setup(|app| {
            // 初始化 AppState（异步块在 setup 内同步执行）
            let handle = app.handle().clone();

            // 用 tokio runtime 同步初始化各 Service
            let rt = tokio::runtime::Runtime::new()?;
            let app_state = rt.block_on(async {
                let event_bus = std::sync::Arc::new(event_bus::EventBus::new());

                // Config 初始化
                let config_path = paths::config_path();
                tracing::info!(?config_path, "loading config");
                let config = std::sync::Arc::new(
                    ConfigService::load(config_path, (*event_bus).clone())
                        .await
                        .expect("failed to load config"),
                );

                // LogStore 初始化（WAL 模式 + 启动 7 天清理后台 task）
                let log_db = paths::log_db_path();
                tracing::info!(?log_db, "initializing logstore");
                let log_store = std::sync::Arc::new(
                    LogStoreService::new(Some(log_db))
                        .await
                        .expect("failed to init LogStore"),
                );

                // **v1.8 / F36**：启动恢复——把上次崩溃时残留的 running 任务标记为 failed
                // 场景：torch 装到一半启动器被重启，DB 里 status='running' 永远不结束
                // 启动时清理，避免历史记录卡在 running
                if let Err(e) = log_store
                    .tasks()
                    .fail_orphaned_running_tasks("启动器启动，上次未完成的任务被强制标记为失败")
                    .await
                {
                    tracing::warn!(error = %e, "F36: fail_orphaned_running_tasks failed");
                }

                // uv sidecar 先发布到用户目录（同时供 PythonEnv + EnvironmentInspector 使用）
                //
                // v2.10：EnvironmentInspector 也需要 uv_binary 用于加速 pip list 探查
                // （uv pip list 主路径 + python -m pip fallback）
                let uv_path = uv_sidecar::ensure_released(&handle).await;

                // EnvironmentInspector 初始化（30s 缓存 + 事件总线订阅 + uv binary 注入）
                // F32：注入 AppHandle，支持 spawn_refresh 完成后 emit `env_inspect_updated` 事件
                let env_inspector = match &uv_path {
                    Some(p) => {
                        tracing::info!(?p, "EnvironmentInspector: injecting uv binary for pip list");
                        EnvironmentInspectorService::new_with_app(
                            (*event_bus).clone(),
                            p.clone(),
                            handle.clone(),
                        )
                    }
                    None => {
                        tracing::warn!(
                            "EnvironmentInspector: uv sidecar not available, pip list will use python -m pip fallback"
                        );
                        // F32: uv 不可用时仍注入 AppHandle（仅丢失 uv pip list 加速，不影响事件推送）
                        EnvironmentInspectorService::new_with_app_optional(
                            (*event_bus).clone(),
                            None,
                            handle.clone(),
                        )
                    }
                };

                // PythonEnvManager 初始化：复用同一 uv_path
                let python_env = match uv_path {
                    Some(p) => {
                        tracing::info!(?p, "using bundled uv sidecar");
                        PythonEnvService::new(p, (*event_bus).clone())
                    }
                    None => {
                        tracing::warn!(
                            "uv sidecar not available, falling back to PATH lookup (user must install uv)"
                        );
                        PythonEnvService::from_path((*event_bus).clone())
                    }
                };

                // CoreManager 初始化（路径热加载：从 config 读 comfyui_root，无需重启即可生效）
                let core_manager = CoreManagerService::new(
                    config.clone(),
                    (*event_bus).clone(),
                    log_store.clone(),
                );

                // ModelPathService 初始化（yaml 路径 = <comfyui_root>/extra_model_paths.yaml，路径热加载）
                let model_path = ModelPathService::new(config.clone());

                // PluginManager 初始化（custom_nodes + venv，路径热加载）
                let plugin_manager = PluginManagerService::new(
                    config.clone(),
                    (*event_bus).clone(),
                );

                // TaskScheduler 初始化（max_concurrent=3 / max_queued=20 默认值，可经 Config 扩展字段覆盖）
                // 设计文档 §2.1 提到可经 [advanced].max_concurrent_tasks 配置，但当前 Config 模块未含此字段，
                // 后续若加配置可在此读取。本期硬编码默认值。
                let task_scheduler = TaskSchedulerService::new(
                    3,
                    20,
                    handle.clone(),
                    log_store.clone(),
                );

                // ProcessLauncher 初始化
                // - comfyui_root / venv_path 由 config 运行时提供（路径热加载）
                // - data_dir 用于存放 comfyui.pid（崩溃恢复用），属于 app 状态不通过 config 改
                let data_dir = paths::app_data_dir();
                if let Err(e) = paths::ensure_dir(&data_dir).await {
                    tracing::warn!(?data_dir, error = %e, "failed to ensure data_dir");
                }
                let python_env_arc = std::sync::Arc::new(python_env);
                let model_path_arc = std::sync::Arc::new(model_path);
                let process_launcher = ProcessLauncherService::new(
                    python_env_arc.clone(),
                    model_path_arc.clone(),
                    log_store.clone(),
                    config.clone(),
                    data_dir,
                );

                // 启动时检测上次未正常退出的 ComfyUI 进程（F21 崩溃恢复）
                // 详见 PR/03-模块设计/06-ProcessLauncher.md §10
                // ProcessLauncherService 内部为 Arc<Inner>，clone 廉价
                let launcher_for_stale = process_launcher.clone();
                let app_handle_for_stale = handle.clone();
                tauri::async_runtime::spawn(async move {
                    launcher_for_stale.check_stale_process(&app_handle_for_stale).await;
                });

                // v3.5：把 TaskScheduler 包装为 Arc 复用（ShutdownCoordinator 和 AppState 各持一份）
                let task_scheduler_arc = std::sync::Arc::new(task_scheduler);

                // F24 退出流程：构造 ShutdownCoordinator（依赖 process_launcher + event_bus + task_scheduler）
                // v3.5：新增 task_scheduler 参数，用于退出时取消阻塞的版本切换类任务
                let shutdown_coordinator = std::sync::Arc::new(ShutdownCoordinator::new(
                    process_launcher.clone(),
                    (*event_bus).clone(),
                    task_scheduler_arc.clone(),
                ));

                // v3.7：transformers 版本索引（PyPI 拉取 + 三层缓存）
                // 缓存文件位于 app_data_dir/transformers_versions.json
                // 注：此处仅构造，spawn_refresh 在 manage 之后调用（避免 manage 前 spawn 访问 State 失败）
                let transformers_cache_file = paths::app_data_dir().join("transformers_versions.json");
                let transformers_index = std::sync::Arc::new(TransformersVersionIndex::new(
                    transformers_cache_file,
                    (*event_bus).clone(),
                    handle.clone(),
                ));

                app_state::AppState {
                    event_bus,
                    config: config.clone(),
                    log_store,
                    env_inspector: std::sync::Arc::new(env_inspector),
                    python_env: python_env_arc,
                    core_manager: std::sync::Arc::new(core_manager),
                    model_path: model_path_arc,
                    plugin_manager: std::sync::Arc::new(plugin_manager),
                    task_scheduler: task_scheduler_arc,
                    process_launcher: std::sync::Arc::new(process_launcher),
                    shutdown_coordinator,
                    transformers_index,
                }
            });

            handle.manage(app_state);
            tracing::info!("Tauri app initialized");

            // v3.7：启动后台拉取 transformers 版本索引（非阻塞，spawn 内部用 tokio::spawn）
            // 放在 manage 之后：spawn_refresh 内部若需访问 AppState（当前未访问，但保留扩展性）可从 State 取
            {
                let state = handle.state::<app_state::AppState>();
                state.transformers_index.spawn_refresh();
            }

            // 初始化系统托盘（详见 tray.rs）
            if let Err(e) = tray::setup(&handle) {
                tracing::warn!(error = %e, "failed to setup system tray");
                // 托盘初始化失败不阻塞应用启动（用户仍可通过窗口操作）
            }

            // v1.8 / F36-Phase2：启动一次性 numpy 迁移
            //
            // 背景：numpy 2.4.4 wheel 缺 exceptions.py（2025-12 报告），导致 `import torch` 失败。
            // 之前 probe_torch 脚本 `except ImportError: installed=False` 把这个错误吞了，
            // 用户看到"已装但显示未装"。本次启动器启动时静默跑一次：
            // 1. 探测 torch import 是否 OK（能 → 不需要迁移）
            // 2. 检查 numpy 版本（>= 2.3 → 已知坏版本）
            // 3. 降级 numpy 到 < 2.3 + smoke test
            // 4. emit RequirementsInstalled 让 env cache 失效
            //
            // 幂等：检测到 numpy < 2.3 立即返回，重启器反复启动也只跑一次降级（pip 已装的就是好版本）。
            // 非阻塞：spawn 后立即返回，不阻塞 setup hook。
            // 错误隔离：迁移失败时仅 warn 日志，不影响启动器正常运行（用户可手动触发 env_repair）。
            {
                let app_handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    // 延迟 3 秒执行，等 EnvironmentInspector 完成首次探查
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    let state = app_handle.state::<app_state::AppState>();
                    let config_snapshot = (**state.config.get()).clone();
                    let venv_path = std::path::PathBuf::from(&config_snapshot.paths.venv_path);
                    let comfyui_root =
                        std::path::PathBuf::from(&config_snapshot.paths.comfyui_root);

                    match crate::python_env::recovery::run_startup_numpy_migration(
                        &state.python_env,
                        &venv_path,
                        &comfyui_root,
                        &config_snapshot,
                        &state.event_bus,
                    )
                    .await
                    {
                        Ok(()) => {
                            tracing::debug!("startup numpy migration: completed (no-op or success)");
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "startup numpy migration failed (non-fatal)");
                        }
                    }
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Config
            commands::config::config_get,
            commands::config::config_launcher_working_dir,
            commands::config::config_update,
            commands::config::config_reset,
            // LogStore
            commands::log_store::log_query,
            commands::log_store::log_tail,
            commands::log_store::log_clear,
            commands::log_store::task_history_list,
            // EnvironmentInspector
            commands::env_inspector::env_inspect,
            // F32: env_probe_torch 已删除（死代码，前端无调用）
            commands::env_inspector::env_list_dependencies,
            commands::env_inspector::env_invalidate_cache,
            commands::env_inspector::env_readiness_check,
            commands::env_inspector::env_check_dependency_conflicts,
            // PythonEnvManager
            commands::python_env::env_status,
            commands::python_env::env_uv_available,
            commands::python_env::env_create_venv,
            commands::python_env::env_install_torch,
            commands::python_env::env_install_requirements,
            commands::python_env::env_switch_python,
            commands::python_env::env_check_compatibility,
            commands::python_env::env_rebuild_venv,
            // v3.0 新增（F25）：torch 多厂商切换
            commands::python_env::env_change_torch_variant,
            // v1.8 / F36-Phase2：环境诊断 + 修复
            commands::python_env::env_diagnose,
            commands::python_env::env_repair,
            // v3.10：torch 一致性诊断（mismatch 检测）
            commands::python_env::env_check_torch_consistency,
            // v3.10：强制一致重装 torch（修复 venv 状态混乱）
            commands::python_env::env_repair_consistent,
            // v3.7：transformers 版本切换
            commands::python_env::env_list_transformers_versions,
            commands::python_env::env_switch_transformers,
            commands::python_env::env_restore_transformers_default,
            // CoreManager
            commands::core_manager::core_clone,
            commands::core_manager::core_ensure_cloned,
            commands::core_manager::core_list_tags,
            commands::core_manager::core_list_tags_classified,
            commands::core_manager::core_check_switch_prerequisites,
            commands::core_manager::core_switch_version,
            commands::core_manager::core_checkout,
            commands::core_manager::core_update,
            commands::core_manager::core_status,
            commands::core_manager::core_is_cloned,
            commands::core_manager::core_ensure_models_link,
            // F31：仓库地址切换与备份恢复
            commands::core_manager::core_get_repo_url,
            commands::core_manager::core_official_repo_url,
            commands::core_manager::core_list_backups,
            commands::core_manager::core_set_repo_url,
            commands::core_manager::core_restore_backup,
            commands::core_manager::core_open_comfyui_dir,
            // F35-A+：工作区脏原因检查 + 一键清理
            commands::core_manager::core_workspace_dirty_reason,
            // F36：venv 路径校验
            config::service::config_validate_venv_path,
            commands::core_manager::core_reset_staged,
            commands::core_manager::core_force_clean_workspace,
            // F36：版本兼容性预检（切版本前弹对话框用）
            commands::core_manager::core_check_version_compatibility,
            // ModelPathManager
            commands::model_path::modelpath_generate,
            commands::model_path::modelpath_remove,
            commands::model_path::modelpath_scan,
            commands::model_path::modelpath_validate,
            // PluginManager
            commands::plugin_manager::plugin_list,
            commands::plugin_manager::plugin_install,
            commands::plugin_manager::plugin_update,
            commands::plugin_manager::plugin_uninstall,
            commands::plugin_manager::plugin_toggle,
            commands::plugin_manager::plugin_install_requirements,
            commands::plugin_manager::plugin_check_updates,
            commands::plugin_manager::plugin_info,
            // TaskScheduler
            commands::task_scheduler::task_list,
            commands::task_scheduler::task_cancel,
            commands::task_scheduler::task_get,
            // ProcessLauncher
            commands::process_launcher::process_start,
            commands::process_launcher::process_stop,
            commands::process_launcher::process_status,
            commands::process_launcher::process_tail_log,
            commands::process_launcher::process_kill_stale,
            commands::process_launcher::shutdown_all,
            // v3.0 新增：System 模块（GPU 检测 + 智能推荐）
            commands::system::system_detect_gpus,
            commands::system::system_clear_gpu_cache,
            commands::system::system_recommend_torch,
            commands::system::system_check_driver_compat,
            // Dev 诊断
            commands::dev_log::dev_log,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
