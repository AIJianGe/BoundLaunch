//! ComfyUI 版本切换任务（v3.5 全面异步化）
//!
//! ## 设计模式
//! - **Template Method**：固定流程，每步可独立失败 + 回滚
//! - **Strategy**：根据失败阶段选择不同回滚策略
//! - **Command**：通过 `make_switch_version_task` 工厂构造 TaskDef
//!
//! ## v3.5 关键改进
//! - **彻底异步化**：步骤 8/9/10（创建 venv / 装 torch / 装 requirements）改为 submit 子任务
//!   - 父任务不 `await` 长操作，通过 `submit_and_wait_with_cancel` 包装
//!   - 父任务被 cancel 时主动 `task_scheduler.cancel(child_id)` 级联取消
//! - **无 timeout**：所有操作支持 `CancellationToken`，**没有 `tokio::time::timeout`**
//! - **实时日志推送**：子进程 stdout/stderr 通过 `LineCollector` 推送到前端
//! - **细粒度进度**：15 段进度（参考 §11.13 启动异步化）
//!
//! ## 流程（12 步）
//! 1. 前置检查（5 步：ComfyUI 已停止 + 工作区干净 + 当前 tag + 模式决策）
//! 2. 解除 models 软链接
//! 3. fetch 远程 tag（决策 10：本地优先）
//! 4. checkout 到目标 tag
//! 5. 重新建立 models 软链接
//! 6. 删除旧 venv（Clean 模式）
//! 7. 创建 venv（submit 子任务）
//! 8. 安装 torch（submit 子任务）
//! 9. 安装 requirements（submit 子任务）
//! 10. 验证 venv 完整性
//! 11. 同步预热 env snapshot_cache（v3.x 新增：解决"切完版本首页仍显示未就绪"问题）
//! 12. 发出 CoreVersionSwitched 事件
//!
//! ## 回滚策略
//! 步骤 4 之后任一失败 → force_checkout 回原 tag
//!
//! 详见 `PR/03-模块设计/03-CoreManager.md §7 版本切换任务`（v3.5 改造）

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::AppHandle;
use tokio_util::sync::CancellationToken;

use crate::common::line_collector::LineCollector;
use crate::config::ConfigService;
use crate::core_manager::compat::SwitchMode;
use crate::core_manager::git_ops;
use crate::core_manager::git_ops_async;
use crate::core_manager::models::SwitchVersionResult;
use crate::core_manager::paths as link_paths;
use crate::env_inspector::EnvironmentInspectorService;
use crate::error::CoreError;
use crate::event_bus::EventBus;
use crate::log_store::LogStoreService;
use crate::process_launcher::ProcessLauncherService;
use crate::python_env::PythonEnvService;
use crate::task_scheduler::progress::ProgressSender;
use crate::task_scheduler::{TaskDef, TaskKind, TaskResult};
use crate::task_scheduler::TaskSchedulerService;

/// 切换版本参数
#[derive(Debug, Clone)]
pub struct SwitchVersionParams {
    pub target_tag: String,
    pub mode: SwitchMode,
}

/// 切换版本任务上下文
pub struct SwitchContext {
    pub config: Arc<ConfigService>,
    pub event_bus: Arc<EventBus>,
    pub log_store: Arc<LogStoreService>,
    pub python_env: Arc<PythonEnvService>,
    pub process_launcher: Arc<ProcessLauncherService>,
    /// v3.5 新增：用于 submit 子任务
    pub task_scheduler: Arc<TaskSchedulerService>,
    /// v3.x 新增：用于在切换末尾同步预热 env snapshot_cache
    ///
    /// 切换完成（git checkout + venv 重建 + torch + requirements 都装好）后，
    /// 调 `force_snapshot_update` 同步跑一次完整探查，把结果写回 snapshot_cache
    /// 并 emit `env_inspect_updated` 事件，避免用户切到首页时还看到 5-30s 前的旧快照。
    /// 详见 `env_inspector::EnvironmentInspectorService::force_snapshot_update`。
    pub env_inspector: Arc<EnvironmentInspectorService>,
}

/// 构造切换版本任务（提交给 TaskScheduler）
///
/// **v3.5**：通过 factory 构造（保留 thin wrapper 以保持向后兼容）
#[deprecated(note = "请使用 crate::task_scheduler::factory::make_switch_version_task")]
pub fn build_switch_task(_params: SwitchVersionParams, _ctx: SwitchContext) -> TaskDef {
    // 占位：实际应传 app_handle，但 build_switch_task 是旧接口，命令层应改调 factory
    // 为编译通过，构造一个空 AppHandle 不可行（AppHandle 没有无参构造）
    // 这里用 panic 提示用户改用 factory
    panic!("build_switch_task 已废弃，请改用 factory::make_switch_version_task 并传入 app_handle")
}

/// 构造切换版本任务（提交给 TaskScheduler）- 旧版入口
///
/// v3.5 之前：此函数返回 TaskDef
/// v3.5 之后：deprecated，改用 `crate::task_scheduler::factory::make_switch_version_task`
pub fn make_switch_version_task(params: SwitchVersionParams, ctx: SwitchContext) -> TaskDef {
    TaskDef {
        kind: TaskKind::Checkout,
        name: format!("切换 ComfyUI 版本到 {}", params.target_tag),
        priority: None, // default = High
        parent_id: None,
        action: Box::new(move |cancel_token: CancellationToken, progress: ProgressSender| {
            let params = params.clone();
            let ctx = ctx;
            Box::pin(async move {
                // v3.5 旧版入口：没有 AppHandle，工厂里的 make_switch_torch_variant_task 需要
                // 这里用 warn + fallback 到 install_torch
                tracing::warn!("make_switch_version_task 旧版入口被调用，建议改用 factory::make_switch_version_task 并传入 app_handle");
                run_switch_version_legacy(params, ctx, cancel_token, progress).await
            })
        }),
    }
}

/// v3.5 旧版 switch_version 主流程（无 AppHandle）
///
/// **仅保留** 供旧的 make_switch_version_task 调用，命令层应改用 factory::make_switch_version_task
async fn run_switch_version_legacy(
    params: SwitchVersionParams,
    ctx: SwitchContext,
    cancel_token: CancellationToken,
    progress: ProgressSender,
) -> Result<TaskResult, String> {
    // 没 AppHandle 时无法用 make_switch_torch_variant_task，
    // 改用 make_install_torch_task（CudaVersion 路径）
    run_switch_version_impl(params, ctx, None, cancel_token, progress).await
}

/// v3.5 新版 switch_version 主流程（带 AppHandle）
///
/// **正式入口**：由 factory::make_switch_version_task 调用
pub async fn run_switch_version(
    params: SwitchVersionParams,
    ctx: SwitchContext,
    app: AppHandle,
    cancel_token: CancellationToken,
    progress: ProgressSender,
) -> Result<TaskResult, String> {
    run_switch_version_impl(params, ctx, Some(app), cancel_token, progress).await
}

/// switch_version 主流程（v3.5 全面异步化）
///
/// **app_handle 为 Some**：走 make_switch_torch_variant_task 路径（带 torch variant 切换）
/// **app_handle 为 None**：走 make_install_torch_task 路径（fallback，仅 CudaVersion）
async fn run_switch_version_impl(
    params: SwitchVersionParams,
    ctx: SwitchContext,
    app_handle: Option<AppHandle>,
    cancel_token: CancellationToken,
    progress: ProgressSender,
) -> Result<TaskResult, String> {
    progress.send_percent(2);
    progress.send_message(format!("开始切换到 {}", params.target_tag));

    // 实时日志收集器（供所有 git / uv 操作使用）
    // ✅ P0-2 修复：创建 collector 同时拿到 rx，启动 forwarder 把日志推到 ProgressSender
    // 这样父任务的 git status / fetch / checkout 输出会实时显示在前端
    let (log_collector, log_rx) = LineCollector::new(500);
    // ✅ P0-2 修复：clone 一份给 forwarder，避免后续 send_message/send_percent 报 moved 错
    let progress_for_forwarder = progress.clone();
    let _log_forwarder = tokio::spawn(async move {
        let mut rx = log_rx;
        while let Some(line) = rx.recv().await {
            progress_for_forwarder.send_log(line.source, line.text);
        }
        tracing::debug!("switcher log forwarder exited");
    });

    // ========== 步骤 1：前置检查 ==========
    progress.send_message("检查 ComfyUI 进程状态".to_string());
    let is_running = ctx.process_launcher.status().await.is_alive();
    if is_running {
        return Err("ComfyUI 正在运行，请先停止后再切换版本".to_string());
    }
    if cancel_token.is_cancelled() {
        return Err("用户取消".to_string());
    }
    progress.send_percent(5);

    // ========== 步骤 2：读取当前状态 ==========
    progress.send_message("读取当前版本信息".to_string());
    let config_snapshot = ctx.config.get();
    let comfyui_root: PathBuf = config_snapshot.paths.comfyui_root.clone();
    let venv_path: PathBuf = config_snapshot.paths.venv_path.clone();
    let models_path: Option<PathBuf> = config_snapshot.paths.models_path.clone();
    let python_version = config_snapshot.paths.python_version.clone();
    drop(config_snapshot);

    if !comfyui_root.join(".git").exists() {
        return Err("ComfyUI 仓库未克隆".to_string());
    }
    progress.send_percent(8);

    // 工作区脏检查（v3.5 改用 git status --porcelain async）
    progress.send_message("检查工作区状态（git status）".to_string());
    let has_local_changes = git_ops_async::git_status_porcelain(
        &comfyui_root,
        &cancel_token,
        log_collector.clone(),
    )
    .await
    .map_err(|e| format!("检查工作区状态失败: {}", e))?;

    if has_local_changes {
        // 给出诊断信息（基于 git status 输出）
        let tail = log_collector.snapshot(20).join("\n");
        return Err(format!(
            "工作区有未提交改动，无法切换版本。\n\
             请在 ComfyUI 目录执行：\n\
             - 查看: git status\n\
             - 丢弃 working tree 改动（不可恢复）: git checkout .\n\
             - 暂存（可恢复）: git stash push -u\n\n\
             git status 输出：\n{}",
            tail
        ));
    }
    progress.send_message("工作区干净，开始切换".to_string());
    progress.send_percent(9);

    // 读取当前 tag（用于回滚）
    let root_for_tag = comfyui_root.clone();
    let original_tag: Option<String> = {
        let root = root_for_tag.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<String>, CoreError> {
            let repo = git_ops::open_repo(&root)?;
            git_ops::current_tag(&repo)
        })
        .await
        .map_err(|e| format!("读取当前 tag 失败: {}", e))?
        .map_err(|e| e.to_string())?
    };

    progress.send_message(format!(
        "当前版本: {}",
        original_tag.as_deref().unwrap_or("(未在 tag 上)")
    ));
    progress.send_percent(10);

    // ========== v1.8 关键改进：降级时强制 Clean 模式 ==========
    use crate::core_manager::semver::cmp_tag_desc;
    let effective_mode = if matches!(params.mode, SwitchMode::Preserve) {
        match (
            original_tag.as_deref(),
            cmp_tag_desc(original_tag.as_deref().unwrap_or(""), &params.target_tag),
        ) {
            (Some(from), std::cmp::Ordering::Greater) => {
                progress.send_message(format!(
                    "⚠ 检测到版本降级（{} → {}），自动切换到 Clean 模式",
                    from, params.target_tag
                ));
                SwitchMode::Clean
            }
            _ => params.mode,
        }
    } else {
        params.mode
    };
    tracing::info!(
        user_mode = ?params.mode,
        effective_mode = ?effective_mode,
        from = ?original_tag,
        to = %params.target_tag,
        "switch_version mode decision"
    );

    // ========== 步骤 3：解除 models 软链接 ==========
    progress.send_message("解除 models 软链接".to_string());
    let link_in_repo = comfyui_root.join("models");
    if link_paths::is_link(&link_in_repo) {
        link_paths::remove_link(&link_in_repo)
            .map_err(|e| format!("解除 models 链接失败: {}", e))?;
    }
    progress.send_percent(15);

    if cancel_token.is_cancelled() {
        let _ = restore_models_link(&comfyui_root, models_path.as_deref());
        return Err("用户取消".to_string());
    }

    // ========== 步骤 4：fetch 远程 tag（决策 10：本地优先） ==========
    let target_tag = params.target_tag.clone();
    let root_for_check = comfyui_root.clone();
    let local_has_tag = tokio::task::spawn_blocking(move || -> Result<bool, CoreError> {
        let repo = git_ops::open_repo(&root_for_check)?;
        let tags = repo
            .tag_names(None)
            .map_err(|e| CoreError::GitError(e.to_string()))?;
        Ok(tags.iter().flatten().any(|n| n == target_tag))
    })
    .await
    .map_err(|e| format!("检查本地 tag 失败: {}", e))?
    .map_err(|e| e.to_string())?;

    if !local_has_tag {
        progress.send_message("本地未找到目标 tag，正在从远程拉取".to_string());
        let fetch_result =
            git_ops_async::fetch_tags_async(&comfyui_root, &cancel_token, log_collector.clone())
                .await;
        if let Err(e) = fetch_result {
            // fetch 失败不视为致命（用户可能切到本地已有 tag）
            tracing::warn!(error = %e, "fetch tags failed, will try local checkout anyway");
            progress.send_message(format!("警告: 拉取远程 tag 失败（{}），尝试用本地", e));
        }
    } else {
        progress.send_message("本地已存在目标 tag".to_string());
    }
    progress.send_percent(25);

    // ========== 步骤 5：检查目标 tag 是否存在 ==========
    let target_tag_check = params.target_tag.clone();
    let root_for_verify = comfyui_root.clone();
    let tag_exists = tokio::task::spawn_blocking(move || -> Result<bool, CoreError> {
        let repo = git_ops::open_repo(&root_for_verify)?;
        let tags = repo
            .tag_names(None)
            .map_err(|e| CoreError::GitError(e.to_string()))?;
        Ok(tags.iter().flatten().any(|n| n == target_tag_check))
    })
    .await
    .map_err(|e| format!("验证 tag 存在失败: {}", e))?
    .map_err(|e| e.to_string())?;

    if !tag_exists {
        let _ = restore_models_link(&comfyui_root, models_path.as_deref());
        return Err(format!("目标 tag {} 不存在", params.target_tag));
    }
    progress.send_percent(30);

    // ========== 步骤 6：checkout 到目标 tag（async + cancel） ==========
    progress.send_message(format!("checkout 到 {}", params.target_tag));
    let checkout_result = git_ops_async::force_checkout_async(
        &comfyui_root,
        &params.target_tag,
        &cancel_token,
        log_collector.clone(),
    )
    .await;

    if let Err(e) = checkout_result {
        let _ = restore_models_link(&comfyui_root, models_path.as_deref());
        return Err(format!("checkout 失败: {}", e));
    }
    progress.send_percent(40);

    // ========== 步骤 7：重新建立 models 软链接 ==========
    let models_link_rebuilt =
        match link_paths::ensure_models_link(&comfyui_root, models_path.as_deref()) {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!(error = %e, "ensure_models_link failed, rolling back");
                let _ = rollback_checkout_async(
                    &comfyui_root,
                    original_tag.as_deref(),
                    &cancel_token,
                    log_collector.clone(),
                )
                .await;
                let _ = restore_models_link(&comfyui_root, models_path.as_deref());
                return Err(format!("重建 models 链接失败: {}（已回滚到原版本）", e));
            }
        };
    progress.send_percent(45);

    // ========== 步骤 8-10：依 effective_mode 处理 venv ==========
    // v3.5：步骤 8/9/10 改为 submit 子任务（无 await 阻塞）
    let mut requirements_reinstalled = false;
    // v3.10：提前读 cuda_version，Preserve 模式也需要它计算 pytorch_index
    let config_for_pytorch_index = ctx.config.get();
    let cuda_version = config_for_pytorch_index.torch.cuda_version;
    drop(config_for_pytorch_index);
    match effective_mode {
        SwitchMode::Clean => {
            // 删除旧 venv（同步 IO，无 cancel 需求）
            progress.send_message("【全部清除】删除旧 venv".to_string());
            if venv_path.exists() {
                if let Err(e) = tokio::fs::remove_dir_all(&venv_path).await {
                    let _ = rollback_checkout_async(
                        &comfyui_root,
                        original_tag.as_deref(),
                        &cancel_token,
                        log_collector.clone(),
                    )
                    .await;
                    let _ = restore_models_link(&comfyui_root, models_path.as_deref());
                    return Err(format!("删除旧 venv 失败: {}（已回滚 git）", e));
                }
            }
            progress.send_percent(50);

            if cancel_token.is_cancelled() {
                let _ = rollback_checkout_async(
                    &comfyui_root,
                    original_tag.as_deref(),
                    &cancel_token,
                    log_collector.clone(),
                )
                .await;
                let _ = restore_models_link(&comfyui_root, models_path.as_deref());
                return Err("用户取消（git 已回滚，venv 已删除）".to_string());
            }

            // ===== 步骤 8：创建 venv（submit 子任务，30→50%） =====
            progress.send_message("创建新 venv（提交子任务）".to_string());
            let venv_factory = crate::task_scheduler::factory::make_create_venv_task(
                ctx.python_env.clone(),
                venv_path.clone(),
                python_version.clone(),
            );
            if let Err(e) = submit_and_wait_with_cancel(
                ctx.task_scheduler.clone(),
                venv_factory,
                &cancel_token,
                &progress,
                (50, 60),
                &log_collector,
                app_handle.as_ref(),
            )
            .await
            {
                let _ = rollback_checkout_async(
                    &comfyui_root,
                    original_tag.as_deref(),
                    &cancel_token,
                    log_collector.clone(),
                )
                .await;
                let _ = restore_models_link(&comfyui_root, models_path.as_deref());
                return Err(format!("创建 venv 失败: {}（已回滚 git）", e));
            }
            progress.send_percent(60);

            // ===== 步骤 9：安装 torch（submit 子任务，60→75%） =====
            if cancel_token.is_cancelled() {
                let _ = rollback_checkout_async(
                    &comfyui_root,
                    original_tag.as_deref(),
                    &cancel_token,
                    log_collector.clone(),
                )
                .await;
                let _ = restore_models_link(&comfyui_root, models_path.as_deref());
                return Err("用户取消（git 已回滚，venv 已建但 torch 未装）".to_string());
            }

            let config_for_install = ctx.config.get();
            // v3.10：cuda_version 已在外层 match 之前读取，这里只读 torch_variant_json
            let torch_variant_json = config_for_install.torch.torch_variant.clone();
            drop(config_for_install);

            progress.send_message("安装 torch（提交子任务）".to_string());
            let torch_task = if let Some(variant_json) = torch_variant_json {
                match serde_json::from_str::<crate::python_env::TorchVariant>(&variant_json) {
                    Ok(variant) => {
                        if let Some(ref app) = app_handle {
                            // v3.5：有 AppHandle 时走 switch_torch_variant_task（带 torch variant 切换）
                            crate::task_scheduler::factory::make_switch_torch_variant_task(
                                ctx.python_env.clone(),
                                ctx.config.clone(),
                                venv_path.clone(),
                                variant,
                                ctx.process_launcher.clone(),
                                app.clone(),
                            )
                        } else {
                            // Fallback：没 AppHandle 时走 install_torch_task（CudaVersion 路径）
                            tracing::warn!("switcher without app_handle, falling back to install_torch");
                            crate::task_scheduler::factory::make_install_torch_task(
                                ctx.python_env.clone(),
                                venv_path.clone(),
                                cuda_version,
                            )
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "parse torch_variant failed, falling back to install_torch");
                        crate::task_scheduler::factory::make_install_torch_task(
                            ctx.python_env.clone(),
                            venv_path.clone(),
                            cuda_version,
                        )
                    }
                }
            } else {
                crate::task_scheduler::factory::make_install_torch_task(
                    ctx.python_env.clone(),
                    venv_path.clone(),
                    cuda_version,
                )
            };

            if let Err(e) = submit_and_wait_with_cancel(
                ctx.task_scheduler.clone(),
                torch_task,
                &cancel_token,
                &progress,
                (60, 75),
                &log_collector,
                app_handle.as_ref(),
            )
            .await
            {
                let _ = rollback_checkout_async(
                    &comfyui_root,
                    original_tag.as_deref(),
                    &cancel_token,
                    log_collector.clone(),
                )
                .await;
                let _ = restore_models_link(&comfyui_root, models_path.as_deref());
                return Err(format!("安装 torch 失败: {}（已回滚 git）", e));
            }
            progress.send_percent(75);

            // ===== 步骤 10：安装 requirements（submit 子任务，75→95%） =====
            if cancel_token.is_cancelled() {
                let _ = rollback_checkout_async(
                    &comfyui_root,
                    original_tag.as_deref(),
                    &cancel_token,
                    log_collector.clone(),
                )
                .await;
                let _ = restore_models_link(&comfyui_root, models_path.as_deref());
                return Err("用户取消（git 已回滚，torch 已装但 requirements 未装）".to_string());
            }

            progress.send_message("安装 requirements.txt（提交子任务）".to_string());
            let req_file = comfyui_root.join("requirements.txt");
            if req_file.exists() {
                // v3.10：传 pytorch_index，防止 transformers 5.x 等依赖触发 torch 覆盖成 +cpu
                let pytorch_index = crate::python_env::uv_runner::cuda_index_url(&cuda_version);
                let req_task = crate::task_scheduler::factory::make_install_requirements_task(
                    ctx.python_env.clone(),
                    venv_path.clone(),
                    req_file,
                    pytorch_index,
                );
                if let Err(e) = submit_and_wait_with_cancel(
                    ctx.task_scheduler.clone(),
                    req_task,
                    &cancel_token,
                    &progress,
                    (75, 95),
                    &log_collector,
                    app_handle.as_ref(),
                )
                .await
                {
                    let _ = rollback_checkout_async(
                        &comfyui_root,
                        original_tag.as_deref(),
                        &cancel_token,
                        log_collector.clone(),
                    )
                    .await;
                    let _ = restore_models_link(&comfyui_root, models_path.as_deref());
                    return Err(format!("安装 requirements 失败: {}（已回滚 git）", e));
                }
                requirements_reinstalled = true;
            } else {
                tracing::warn!("requirements.txt not found in comfyui_root, skip");
                progress.send_message("警告: 未找到 requirements.txt，跳过依赖安装".to_string());
            }
            progress.send_percent(95);
        }
        SwitchMode::Preserve => {
            progress.send_message("【升/降版本】保留 venv，pip install --upgrade --force-reinstall".to_string());
            let req_file = comfyui_root.join("requirements.txt");
            if req_file.exists() {
                // Preserve 模式：直接 await install_requirements_upgrade（不是 submit 子任务，
                // 因为这一步是"复用 venv"的特殊路径，复用现有 python_env 内部实现）
                // v3.10：传 pytorch_index，防止 transformers 5.x 等依赖触发 torch 覆盖成 +cpu
                let pytorch_index = crate::python_env::uv_runner::cuda_index_url(&cuda_version);
                if let Err(e) = ctx
                    .python_env
                    .install_requirements_upgrade(&venv_path, &req_file, pytorch_index.as_deref(), &cancel_token, None)
                    .await
                {
                    return Err(format!("pip install --upgrade 失败: {}（git 已切换）", e));
                }
                requirements_reinstalled = true;
            } else {
                tracing::warn!("requirements.txt not found, skip upgrade");
            }
            progress.send_percent(85);
        }
        SwitchMode::Skip => {
            progress.send_message("【不动环境】只切 git tag，venv 不动".to_string());
            progress.send_percent(90);
        }
    }

    // ========== 步骤 11：验证 venv ==========
    progress.send_message("验证 venv 完整性".to_string());
    let verify_result = ctx.python_env.verify_venv(&venv_path, &cancel_token).await;
    if let Err(e) = verify_result {
        tracing::warn!(error = %e, "verify_venv failed, but git already switched");
        progress.send_message(format!("警告: venv 验证失败: {}（版本切换已完成）", e));
    }
    progress.send_percent(95);

    // ========== 步骤 12：同步预热 env snapshot_cache ==========
    //
    // 目的：用户切完版本回到首页时，立即看到"就绪"和"已安装"，
    // 而不是看到 5-30s 前的旧 snapshot_cache（`spawn_refresh` 异步刷新延迟）。
    //
    // 行为：
    // 1. 同步跑一次完整 inspect_snapshot（probe_torch + inspect_dependencies + detect_gpu）
    // 2. 把结果写回 snapshot_cache
    // 3. emit env_inspect_updated 事件给前端
    //
    // 失败容忍：探查失败不阻塞切换完成，tracing::warn 记录。
    // 此时 `CoreVersionSwitched` 事件仍会 emit，前端会通过后续的
    // `spawn_refresh` 异步刷新来拿到正确状态（与旧行为一致）。
    progress.send_message("同步刷新环境信息（让首页立即看到就绪）".to_string());
    if let Err(e) = ctx
        .env_inspector
        .force_snapshot_update(&venv_path, &comfyui_root, &cancel_token)
        .await
    {
        tracing::warn!(
            error = %e,
            "force_snapshot_update failed after switch, will rely on async spawn_refresh"
        );
        progress.send_message(format!(
            "警告: 同步刷新环境失败（{}），切回首页时可能短暂看到旧状态",
            e
        ));
    }
    progress.send_percent(100);

    // ========== 发出事件 ==========
    ctx.event_bus.emit(crate::event_bus::SystemEvent::CoreVersionSwitched {
        from: original_tag.clone(),
        to: params.target_tag.clone(),
    });

    let summary = format!(
        "已切换 ComfyUI 版本: {} → {}",
        original_tag.as_deref().unwrap_or("(unknown)"),
        params.target_tag
    );

    let payload = serde_json::to_value(SwitchVersionResult::Success {
        from: original_tag.clone(),
        to: params.target_tag.clone(),
        venv_rebuilt: true,
        models_link_rebuilt,
        requirements_reinstalled,
    })
    .map_err(|e| format!("序列化结果失败: {}", e))?;

    Ok(TaskResult {
        summary,
        payload: Some(payload),
    })
}

/// 提交子任务并等待完成（v3.5 嵌套子任务核心）
///
/// 行为：
/// 1. submit_child 子任务（自动注入 parent_id）拿到 child_id
/// 2. **可选**：若有 AppHandle，spawn child_progress_forwarder：
///    - 监听 task_progress 事件并映射到父任务进度段
///    - 监听 task_log 事件并写入父任务 LineCollector
///    - 父 cancel 时级联 cancel 子任务
/// 3. 等待子任务完成（task_scheduler.wait 内部循环查询状态）
/// 4. 父 cancel 时 wait 返回 WaitCancelled
/// 5. 等 child 真的退出后，父返回
async fn submit_and_wait_with_cancel(
    task_scheduler: Arc<TaskSchedulerService>,
    child_task: TaskDef,
    parent_cancel: &CancellationToken,
    parent_progress: &ProgressSender,
    progress_range: (u8, u8), // (段起, 段止)
    parent_log_collector: &Arc<LineCollector>,
    app_handle: Option<&AppHandle>,
) -> Result<TaskResult, String> {
    // ✅ P2-1 修复：用 submit_child 自动注入 parent_id
    //    这样子任务的 task_log 事件带 parent_task_id，前端 useTaskProgress 跟踪父任务时
    //    也能把子任务的 uv/git 进度日志显示到父任务的实时日志面板。
    let parent_task_id = parent_progress.task_id.clone();
    let child_id = if parent_task_id.is_empty() {
        // 测试场景：ProgressSender::no_op() 的 task_id 为空字符串，走普通 submit
        task_scheduler
            .submit(child_task)
            .await
            .map_err(|e| format!("提交子任务失败: {}", e))?
    } else {
        task_scheduler
            .submit_child(child_task, parent_task_id)
            .await
            .map_err(|e| format!("提交子任务失败: {}", e))?
    };

    // v3.5：若 AppHandle 可用，spawn 进度/日志 forwarder
    // （让父任务进度条反映子任务进度，实时日志能合并到父任务的日志面板）
    if let Some(app) = app_handle {
        crate::task_scheduler::factory::spawn_child_progress_forwarder(
            app.clone(),
            task_scheduler.clone(),
            child_id.clone(),
            parent_progress.clone(),
            parent_cancel.clone(),
            parent_log_collector.clone(),
            progress_range,
        );
    }

    // 等待子任务完成
    let wait_result = task_scheduler.wait(&child_id).await;

    match wait_result {
        Ok(result) => {
            parent_progress.send_percent(progress_range.1);
            Ok(result)
        }
        Err(crate::task_scheduler::TaskError::WaitCancelled) => {
            // 父 cancel 触发的子任务取消
            Err("用户取消（子任务已级联取消）".to_string())
        }
        Err(e) => Err(format!("子任务失败: {}", e)),
    }
}

/// F35-C：回滚 checkout 的三级兜底（v3.5 改用 async + LineCollector）
async fn rollback_checkout_async(
    comfyui_root: &std::path::Path,
    original_tag: Option<&str>,
    cancel: &CancellationToken,
    log_collector: Arc<LineCollector>,
) -> Result<(), CoreError> {
    if let Some(tag) = original_tag {
        // 兜底 1：回滚到 original_tag
        let result =
            git_ops_async::force_checkout_async(comfyui_root, tag, cancel, log_collector.clone())
                .await;
        if result.is_ok() {
            tracing::info!(tag, "rollback checkout completed (tier-1: original_tag)");
            return Ok(());
        }
        tracing::warn!(?result, tag, "rollback tier-1 failed");
    }

    // 兜底 2-3 略（保留旧 sync 实现，依赖 spawn_blocking）
    // v3.6 完整迁移到 async
    let root = comfyui_root.to_path_buf();
    let tag_owned = original_tag.unwrap_or("HEAD").to_string();
    tokio::task::spawn_blocking(move || -> Result<(), CoreError> {
        let mut repo = git_ops::open_repo(&root)?;
        git_ops::force_checkout(&mut repo, &tag_owned)
    })
    .await
    .map_err(|e| CoreError::GitError(format!("rollback thread join: {}", e)))?
}

/// 恢复 models 软链接
fn restore_models_link(
    comfyui_root: &std::path::Path,
    models_path: Option<&std::path::Path>,
) -> Result<(), CoreError> {
    link_paths::ensure_models_link(comfyui_root, models_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_switch_version_params_debug() {
        let p = SwitchVersionParams {
            target_tag: "v0.3.10".to_string(),
            mode: SwitchMode::Preserve,
        };
        assert_eq!(p.target_tag, "v0.3.10");
        assert!(matches!(p.mode, SwitchMode::Preserve));
    }
}
