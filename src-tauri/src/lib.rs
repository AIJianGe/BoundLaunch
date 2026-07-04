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
use crate::python_env::PythonEnvService;
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
                let config = ConfigService::load(config_path, (*event_bus).clone())
                    .await
                    .expect("failed to load config");

                // LogStore 初始化（WAL 模式 + 启动 7 天清理后台 task）
                let log_db = paths::log_db_path();
                tracing::info!(?log_db, "initializing logstore");
                let log_store = std::sync::Arc::new(
                    LogStoreService::new(Some(log_db))
                        .await
                        .expect("failed to init LogStore"),
                );

                // EnvironmentInspector 初始化（30s 缓存 + 事件总线订阅）
                let env_inspector =
                    EnvironmentInspectorService::new((*event_bus).clone());

                // PythonEnvManager 初始化：优先 sidecar uv，回退到 PATH
                // 详见 uv_sidecar.rs
                let python_env = match uv_sidecar::ensure_released(&handle).await {
                    Some(uv_path) => {
                        tracing::info!(?uv_path, "using bundled uv sidecar");
                        PythonEnvService::new(uv_path, (*event_bus).clone())
                    }
                    None => {
                        tracing::warn!(
                            "uv sidecar not available, falling back to PATH lookup (user must install uv)"
                        );
                        PythonEnvService::from_path((*event_bus).clone())
                    }
                };

                // CoreManager 初始化（comfyui_root 来自 Config，复用同一个 Arc<LogStoreService>）
                let comfyui_root = std::path::PathBuf::from(&config.get().paths.comfyui_root);
                let core_manager = CoreManagerService::new(
                    comfyui_root.clone(),
                    (*event_bus).clone(),
                    log_store.clone(),
                );

                // ModelPathService 初始化（yaml 路径 = <comfyui_root>/extra_model_paths.yaml）
                let model_path = ModelPathService::new(comfyui_root.clone());

                // PluginManager 初始化（custom_nodes = <comfyui_root>/custom_nodes，venv 来自 Config）
                let custom_nodes_path = comfyui_root.join("custom_nodes");
                let venv_path = std::path::PathBuf::from(&config.get().paths.venv_path);
                let plugin_manager = PluginManagerService::new(
                    custom_nodes_path,
                    venv_path.clone(),
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
                // - 复用 comfyui_root / venv_path（来自 Config）
                // - data_dir 用于存放 comfyui.pid（崩溃恢复用）
                let data_dir = paths::app_data_dir();
                if let Err(e) = paths::ensure_dir(&data_dir).await {
                    tracing::warn!(?data_dir, error = %e, "failed to ensure data_dir");
                }
                let python_env_arc = std::sync::Arc::new(python_env);
                let model_path_arc = std::sync::Arc::new(model_path);
                let config_arc = std::sync::Arc::new(config);
                let process_launcher = ProcessLauncherService::new(
                    python_env_arc.clone(),
                    model_path_arc.clone(),
                    log_store.clone(),
                    config_arc.clone(),
                    comfyui_root.clone(),
                    venv_path.clone(),
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

                app_state::AppState {
                    event_bus,
                    config: config_arc,
                    log_store,
                    env_inspector: std::sync::Arc::new(env_inspector),
                    python_env: python_env_arc,
                    core_manager: std::sync::Arc::new(core_manager),
                    model_path: model_path_arc,
                    plugin_manager: std::sync::Arc::new(plugin_manager),
                    task_scheduler: std::sync::Arc::new(task_scheduler),
                    process_launcher: std::sync::Arc::new(process_launcher),
                }
            });

            handle.manage(app_state);
            tracing::info!("Tauri app initialized");

            // 初始化系统托盘（详见 tray.rs）
            if let Err(e) = tray::setup(&handle) {
                tracing::warn!(error = %e, "failed to setup system tray");
                // 托盘初始化失败不阻塞应用启动（用户仍可通过窗口操作）
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
            commands::env_inspector::env_probe_torch,
            commands::env_inspector::env_list_dependencies,
            commands::env_inspector::env_invalidate_cache,
            commands::env_inspector::env_readiness_check,
            // PythonEnvManager
            commands::python_env::env_status,
            commands::python_env::env_uv_available,
            commands::python_env::env_create_venv,
            commands::python_env::env_install_torch,
            commands::python_env::env_switch_python,
            commands::python_env::env_check_compatibility,
            commands::python_env::env_rebuild_venv,
            // CoreManager
            commands::core_manager::core_clone,
            commands::core_manager::core_ensure_cloned,
            commands::core_manager::core_list_tags,
            commands::core_manager::core_checkout,
            commands::core_manager::core_update,
            commands::core_manager::core_status,
            commands::core_manager::core_is_cloned,
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
