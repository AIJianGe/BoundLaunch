//! ComfyUI 版本切换任务（v3.1 / F26）
//!
//! ## 设计模式
//! - **Template Method**：固定 11 步流程，每步可独立失败 + 回滚
//! - **Strategy**：根据失败阶段选择不同回滚策略
//! - **Command**：通过 `switch_version_def` 工厂构造 TaskDef，提交给 TaskScheduler
//!
//! ## 11 步流程
//! 1. 前置检查：ComfyUI 已停止 + 工作区干净
//! 2. 记录当前 tag（用于回滚）
//! 3. 解除 models 软链接（避免 git checkout 冲突）
//! 4. fetch 远程 tag（决策 10：本地优先，本地没有再拉远程）
//! 5. 检查目标 tag 是否存在
//! 6. checkout 到目标 tag
//! 7. 重新建立 models 软链接
//! 8. 删除旧 venv（决策 3：总是重建）
//! 9. 创建新 venv
//! 10. 安装 torch + smoke test + requirements（带 constraints）
//! 11. 验证 venv 完整性
//!
//! ## v1.8 关键改进
//! - **降级时强制 Clean 模式**：检测 `from > to`（按 SemVer）时覆盖用户选择的 Preserve
//!   强制走 Clean 路径，避免依赖冲突残留
//! - **torch smoke test 集成**：装 torch 后调 `smoke_test_torch()` 验证 import 成功
//! - **freeze constraints**：装 requirements 时叠加 freeze.rs 约束，防止 numpy 2.4.4 等
//!
//! ## 回滚策略（决策 6：全部回滚）
//! 步骤 6/7/8/9/10 任一失败 → 执行回滚：
//! - 解除 models 链接（如果在步骤 7 后失败）
//! - force_checkout 回原 tag
//! - 重建 models 链接
//! - venv 状态可能不一致（已删除但未重建成功）→ 标记 rollback_clean=false
//!
//! 详见 `PR/03-模块设计/03-CoreManager.md §7 版本切换任务`（F26 新增）

use std::path::PathBuf;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::config::ConfigService;
use crate::core_manager::git_ops;
use crate::core_manager::models::{SwitchVersionResult, TagInfo};
use crate::core_manager::paths as link_paths;
use crate::error::CoreError;
use crate::event_bus::EventBus;
use crate::log_store::LogStoreService;
use crate::process_launcher::ProcessLauncherService;
use crate::python_env::PythonEnvService;
use crate::task_scheduler::progress::ProgressSender;
use crate::task_scheduler::{TaskDef, TaskKind, TaskPriority, TaskResult};

/// 切换版本参数
#[derive(Debug, Clone)]
pub struct SwitchVersionParams {
    /// 目标 tag（如 "v0.3.10"）
    pub target_tag: String,
    /// v1.8 / F36：切换模式（用户在对话框选）
    pub mode: crate::core_manager::compat::SwitchMode,
}

/// 切换版本任务上下文
///
/// 集中持有所有依赖服务，便于在闭包中使用
pub struct SwitchContext {
    pub config: Arc<ConfigService>,
    pub event_bus: Arc<EventBus>,
    pub log_store: Arc<LogStoreService>,
    pub python_env: Arc<PythonEnvService>,
    pub process_launcher: Arc<ProcessLauncherService>,
}

/// 构造一个切换版本任务定义，提交给 TaskScheduler
///
/// 调用方（commands/core_manager.rs）在 Tauri command 中：
/// ```ignore
/// let def = build_switch_task(params, ctx);
/// let task_id = state.task_scheduler.submit(def).await?;
/// ```
pub fn build_switch_task(params: SwitchVersionParams, ctx: SwitchContext) -> TaskDef {
    TaskDef {
        kind: TaskKind::Checkout,
        name: format!("切换 ComfyUI 版本到 {}", params.target_tag),
        priority: Some(TaskPriority::High),
        action: Box::new(move |cancel_token: CancellationToken, progress: ProgressSender| {
            let params = params.clone();
            let ctx = ctx;
            Box::pin(async move {
                run_switch_version(params, ctx, cancel_token, progress).await
            })
        }),
    }
}

/// 切换版本主流程
async fn run_switch_version(
    params: SwitchVersionParams,
    ctx: SwitchContext,
    cancel_token: CancellationToken,
    progress: ProgressSender,
) -> Result<TaskResult, String> {
    progress.send_percent(2);
    progress.send_message(format!("开始切换到 {}", params.target_tag));

    // ========== 步骤 1：前置检查 ==========
    progress.send_message("检查 ComfyUI 进程状态".to_string());
    let is_running = ctx.process_launcher.status().await.is_alive();
    if is_running {
        return Err("ComfyUI 正在运行，请先停止后再切换版本".to_string());
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

    // F35-A：前置工作区脏检查（与前端 check_switch_prerequisites 行为一致）
    //
    // 必要性：
    // - 前端 hasLocalChanges 派生自 CoreStatus.has_local_changes，仅做 UI 提示
    // - 后端原始 has_local_changes 严格基于 git status 含 untracked，任何临时残留
    //   （如 models 软链接被中途解除、venv_OLD 残留、custom_nodes/.tmp 目录）都会让状态卡住
    // - 不在任务开始时校验会进入半成功状态，留下 untracked 残留后无法切换（按钮永久灰）
    //
    // 行为：与前端命令同步校验，脏时立即返回，行为一致（前端已阻止的任务后端也拒绝）
    //
    // **F35-A+ (v1.8 增强)**：返回结构化原因（staged / unstaged / untracked），
    // 让前端能告诉用户"具体是哪种改动"以及针对性命令。原因：用户曾卡在 28 个文件
    // staged 改动上，`git diff` 看不到，UI 永远显示"工作区脏"但不知为何。
    let dirty_reason: Option<git_ops::WorkspaceDirtyReason> = {
        let root = comfyui_root.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<git_ops::WorkspaceDirtyReason>, CoreError> {
            let repo = git_ops::open_repo(&root)?;
            Ok(git_ops::inspect_workspace_dirty(&repo))
        })
        .await
        .map_err(|e| format!("检查工作区状态失败: {}", e))?
        .map_err(|e| e.to_string())?
    };
    if let Some(reason) = dirty_reason {
        let hint = match &reason {
            git_ops::WorkspaceDirtyReason::Staged { count, .. } => format!(
                "检测到 {} 个文件已 add 但未 commit（staged 改动）。\n\
                 - 查看：`git diff --cached --stat`\n\
                 - 撤销 staging 保留文件内容（推荐）：`git reset HEAD` 后再用 `git checkout .` 丢弃 working tree 改动\n\
                 - 或前端点击「重置 Staged」按钮一键撤销 staging",
                count
            ),
            git_ops::WorkspaceDirtyReason::Unstaged { count, .. } => format!(
                "检测到 {} 个文件有 working tree 改动。\n\
                 - 查看：`git diff --stat`\n\
                 - 丢弃：`git checkout .`（不可恢复）\n\
                 - 暂存：`git stash push -u`（可恢复）",
                count
            ),
            git_ops::WorkspaceDirtyReason::Untracked { count, .. } => format!(
                "检测到 {} 个 untracked 文件/目录。\n\
                 - 查看：`git status --porcelain | findstr \"??\"`\n\
                 - 清理：`git clean -fd`（不删被忽略的）",
                count
            ),
        };
        return Err(format!("工作区有未提交改动（{}），无法切换版本。\n\n{}", reason.label(), hint));
    }
    progress.send_message("工作区干净，开始切换");
    progress.send_percent(9);

    // 读取当前 tag（用于回滚）
    let original_tag: Option<String> = {
        let root = comfyui_root.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<String>, CoreError> {
            let repo = git_ops::open_repo(&root)?;
            git_ops::current_tag(&repo)
        })
        .await
        .map_err(|e| format!("读取当前 tag 失败: {}", e))
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?
    };

    progress.send_message(format!(
        "当前版本: {}",
        original_tag.as_deref().unwrap_or("(未在 tag 上)")
    ));
    progress.send_percent(10);

    // ========== v1.8 关键改进：降级时强制 Clean 模式 ==========
    //
    // 背景：之前 Preserve 模式（`pip install -r --upgrade --force-reinstall`）在
    // 降级时容易残留坏版本依赖（如 numpy 2.4.4 wheel 缺 exceptions.py），导致
    // `import torch` 失败但 pip 返回 success（前端显示"未安装"）。
    //
    // 解决：检测 SemVer from > to 时，**强制覆盖**用户选择的 Preserve → Clean。
    // 不论用户在对话框选了什么，降级都走"删 venv → 重建 → 装 requirements"路径。
    //
    // 升级（from < to）：尊重用户选择（Preserve 通常更快）
    // 跨版本（如 from v0.2 → to v0.5，含 minor 变化）：仍推荐 Clean
    // patch（from v0.2.1 → to v0.2.2）：Preserve 安全
    use crate::core_manager::compat::SwitchMode;
    use crate::core_manager::semver::cmp_tag_desc;
    let effective_mode = if matches!(params.mode, SwitchMode::Preserve) {
        match (
            original_tag.as_deref(),
            cmp_tag_desc(original_tag.as_deref().unwrap_or(""), &params.target_tag),
        ) {
            (Some(from), std::cmp::Ordering::Greater) => {
                // cmp_tag_desc(from, to) == Greater 表示 from 更新（from > to = 降级）
                progress.send_message(format!(
                    "⚠ 检测到版本降级（{} → {}），自动切换到 Clean 模式（避免依赖冲突残留）",
                    from, params.target_tag
                ));
                SwitchMode::Clean
            }
            _ => params.mode, // 升级或相同版本，尊重用户选择
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

    // 检查取消信号
    if cancel_token.is_cancelled() {
        // 取消时尝试恢复链接
        if let Err(e) = restore_models_link(&comfyui_root, models_path.as_deref()) {
            tracing::warn!(error = %e, "cancel rollback: restore models link failed");
        }
        return Err("任务已取消".to_string());
    }

    // ========== 步骤 4：fetch 远程 tag（决策 10：本地优先） ==========
    // 决策 10：先尝试用本地 tag，本地没有再拉远程
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
        let root_for_fetch = comfyui_root.clone();
        // v3.3 / F33：使用 Config 中的 current_repo_url（F31 切换仓库后保持一致），
        // 而非硬编码 COMFYUI_REPO_URL 常量。
        let fetch_url = ctx
            .config
            .get()
            .paths
            .comfyui_repo_url
            .clone()
            .unwrap_or_else(|| crate::core_manager::models::COMFYUI_REPO_URL.to_string());
        let fetch_result = tokio::task::spawn_blocking(move || -> Result<(), CoreError> {
            let repo = git_ops::open_repo(&root_for_fetch)?;
            git_ops::fetch_tags(&repo, &fetch_url)
        })
        .await
        .map_err(|e| format!("fetch tag 任务失败: {}", e))?;

        if let Err(e) = fetch_result {
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
        // 回滚：恢复 models 链接
        let _ = restore_models_link(&comfyui_root, models_path.as_deref());
        return Err(format!("目标 tag {} 不存在", params.target_tag));
    }
    progress.send_percent(30);

    // ========== 步骤 6：checkout 到目标 tag ==========
    progress.send_message(format!("checkout 到 {}", params.target_tag));
    let root_for_checkout = comfyui_root.clone();
    let tag_for_checkout = params.target_tag.clone();
    let checkout_result = tokio::task::spawn_blocking(move || -> Result<(), CoreError> {
        let mut repo = git_ops::open_repo(&root_for_checkout)?;
        git_ops::force_checkout(&mut repo, &tag_for_checkout)
    })
    .await
    .map_err(|e| format!("checkout 任务失败: {}", e))?;

    if let Err(e) = checkout_result {
        // 回滚：恢复 models 链接（git 状态未变）
        let _ = restore_models_link(&comfyui_root, models_path.as_deref());
        return Err(format!("checkout 失败: {}", e));
    }
    progress.send_percent(40);

    // ========== 步骤 7：重新建立 models 软链接 ==========
    let models_link_rebuilt = match link_paths::ensure_models_link(&comfyui_root, models_path.as_deref()) {
        Ok(_) => true,
        Err(e) => {
            // 回滚：force checkout 回原 tag
            tracing::warn!(error = %e, "ensure_models_link failed, rolling back");
            let _ = rollback_checkout(&comfyui_root, original_tag.as_deref(), &ctx.log_store);
            // 再次尝试恢复链接
            let _ = restore_models_link(&comfyui_root, models_path.as_deref());
            return Err(format!(
                "重建 models 链接失败: {}（已回滚到原版本）",
                e
            ));
        }
    };
    progress.send_percent(45);

    // ========== 步骤 8-10：依 effective_mode 处理 venv ==========
    // v1.8：effective_mode 已在步骤 2 后自动调整（降级强制 Clean）
    let mut requirements_reinstalled = false;
    match effective_mode {
        SwitchMode::Clean => {
            // 删除旧 venv
            progress.send_message("【全部清除】删除旧 venv".to_string());
            if venv_path.exists() {
                if let Err(e) = tokio::fs::remove_dir_all(&venv_path).await {
                    let _ = rollback_checkout(&comfyui_root, original_tag.as_deref(), &ctx.log_store);
                    let _ = restore_models_link(&comfyui_root, models_path.as_deref());
                    return Err(format!("删除旧 venv 失败: {}（已回滚 git）", e));
                }
            }
            progress.send_percent(50);

            // 创建新 venv
            progress.send_message("创建新 venv".to_string());
            if let Err(e) = ctx.python_env.create_venv(&venv_path, &python_version).await {
                let _ = rollback_checkout(&comfyui_root, original_tag.as_deref(), &ctx.log_store);
                let _ = restore_models_link(&comfyui_root, models_path.as_deref());
                return Err(format!("创建新 venv 失败: {}（已回滚 git）", e));
            }
            progress.send_percent(60);

            if cancel_token.is_cancelled() {
                let _ = rollback_checkout(&comfyui_root, original_tag.as_deref(), &ctx.log_store);
                let _ = restore_models_link(&comfyui_root, models_path.as_deref());
                return Err("任务已取消（git 已回滚，venv 已重建但未装依赖）".to_string());
            }

            // 装 torch（自动带 smoke test）
            progress.send_message("安装 torch".to_string());
            let config_for_install = ctx.config.get();
            let cuda_version = config_for_install.torch.cuda_version;
            let torch_variant_json = config_for_install.torch.torch_variant.clone();
            drop(config_for_install);

            let torch_result = if let Some(variant_json) = torch_variant_json {
                match serde_json::from_str::<crate::python_env::TorchVariant>(&variant_json) {
                    Ok(variant) => ctx.python_env.switch_torch_variant(&venv_path, &variant).await,
                    Err(e) => {
                        tracing::warn!(error = %e, "parse torch_variant failed, falling back to cuda_version");
                        ctx.python_env.install_torch(&venv_path, cuda_version).await
                    }
                }
            } else {
                ctx.python_env.install_torch(&venv_path, cuda_version).await
            };

            if let Err(e) = torch_result {
                let _ = rollback_checkout(&comfyui_root, original_tag.as_deref(), &ctx.log_store);
                let _ = restore_models_link(&comfyui_root, models_path.as_deref());
                return Err(format!("安装 torch 失败: {}（已回滚 git）", e));
            }
            progress.send_percent(75);

            // 装 ComfyUI requirements（带 freeze constraints）
            progress.send_message("安装 ComfyUI 依赖 (requirements.txt)".to_string());
            let req_file = comfyui_root.join("requirements.txt");
            if req_file.exists() {
                if let Err(e) = ctx.python_env.install_requirements(&venv_path, &req_file).await {
                    let _ = rollback_checkout(&comfyui_root, original_tag.as_deref(), &ctx.log_store);
                    let _ = restore_models_link(&comfyui_root, models_path.as_deref());
                    return Err(format!("安装 requirements 失败: {}（已回滚 git）", e));
                }
                requirements_reinstalled = true;
            } else {
                tracing::warn!("requirements.txt not found in comfyui_root, skip");
                progress.send_message("警告: 未找到 requirements.txt，跳过依赖安装".to_string());
            }
            progress.send_percent(90);
        }
        SwitchMode::Preserve => {
            // 保留 venv，只 `pip install -r new-req.txt --upgrade --force-reinstall`
            // v1.8：自动应用 freeze constraints（防止 numpy 2.4.4 等）
            progress.send_message("【升/降版本】保留 venv，pip install --upgrade --force-reinstall".to_string());
            let req_file = comfyui_root.join("requirements.txt");
            if req_file.exists() {
                if let Err(e) = ctx.python_env.install_requirements_upgrade(&venv_path, &req_file).await {
                    // Preserve 模式失败不回滚 git（用户可能希望保留 git 切换到新版本）
                    return Err(format!("pip install --upgrade 失败: {}（git 已切换，请检查依赖）", e));
                }
                requirements_reinstalled = true;
            } else {
                tracing::warn!("requirements.txt not found, skip upgrade");
            }
            progress.send_percent(85);
        }
        SwitchMode::Skip => {
            // 不动 venv
            progress.send_message("【不动环境】只切 git tag，venv 不动".to_string());
            progress.send_percent(90);
        }
    }

    // ========== 步骤 11：验证 venv ==========
    progress.send_message("验证 venv 完整性".to_string());
    let verify_result = ctx.python_env.verify_venv(&venv_path).await;
    if let Err(e) = verify_result {
        // 验证失败不回滚 git（venv 已经装好但验证脚本失败），但提示用户
        tracing::warn!(error = %e, "verify_venv failed, but git already switched");
        progress.send_message(format!("警告: venv 验证失败: {}（版本切换已完成，请手动检查）", e));
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

/// F35-C：回滚 checkout 的三级兜底
///
/// 必要性：
/// - 原有实现：仅尝试回滚到 `original_tag`，失败就返回 Err
/// - 失败场景：original_tag 是 detached HEAD 上的临时 commit、tag 已被远程删除、
///   工作区半脏导致 force_checkout 失败 → 仓库留在孤儿 commit 上，
///   `current_tag()` 返回 None → UI 显示"未知" → 后续所有切换失败
///
/// 兜底策略（按优先级）：
/// 1. 优先回滚到 original_tag
/// 2. 失败时尝试回滚到 LogStore 持久化 tags 缓存中的 latest_stable
/// 3. 仍失败时执行 `git reset --hard HEAD~1`，至少回退一步
/// 4. 全部失败时调用 F35-E `emergency_reset_to_head` 清 index，
///    至少保证 working tree 不残留 staged 改动（按钮不会永久灰）
/// 5. F35-E 也失败才返回 Err，附详细说明
fn rollback_checkout(
    comfyui_root: &std::path::Path,
    original_tag: Option<&str>,
    log_store: &Arc<LogStoreService>,
) -> Result<(), CoreError> {
    // 兜底 1：回滚到 original_tag
    if let Some(tag) = original_tag {
        let tag_owned = tag.to_string();
        let tag_for_log = tag_owned.clone();
        let root = comfyui_root.to_path_buf();
        let result = std::thread::spawn(move || -> Result<(), CoreError> {
            let mut repo = git_ops::open_repo(&root).map_err(|e| CoreError::GitError(e.to_string()))?;
            git_ops::force_checkout(&mut repo, &tag_owned)
        })
        .join();
        match result {
            Ok(Ok(())) => {
                tracing::info!(tag = %tag_for_log, "rollback checkout completed (tier-1: original_tag)");
                return Ok(());
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %e, tag = %tag_for_log, "rollback tier-1 failed, trying latest_stable cache");
            }
            Err(e) => {
                tracing::warn!(?e, tag = %tag_for_log, "rollback tier-1 thread panicked");
            }
        }
    } else {
        tracing::warn!("no original_tag, skipping tier-1 rollback");
    }

    // 兜底 2：从 LogStore 持久化 tags 缓存读 latest_stable 回滚
    // 注：log_store 是同步包装，但 load_cached_tags 是 async；我们在 spawn_blocking 中
    // 通过 tokio runtime 拿不到 handle 的场景下用 try_read 同步跑（仅在 tier-2 调用，
    // 对延迟不敏感）。这里采用更简单的方式：同步 spawn 一条独立 runtime。
    let latest_from_cache: Option<String> = {
        let log_store = log_store.clone();
        // 同步执行：使用 std::thread 包装，在另一线程中跑 tokio runtime
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            rt.block_on(async move {
                let entry = log_store.logs().load_cached_tags().await.ok().flatten()?;
                let json = entry.0;
                let tags: Vec<TagInfo> = serde_json::from_str(&json).ok()?;
                // 优先取第一个稳定版（缓存本身就是按 SemVer 倒序排的，F33 之后）
                tags.iter()
                    .find(|t| t.is_stable)
                    .map(|t| t.name.clone())
                    .or_else(|| tags.first().map(|t| t.name.clone()))
            })
        })
        .join()
        .ok()
        .flatten()
    };
    if let Some(latest) = latest_from_cache {
        let latest_log = latest.clone();
        let root3 = comfyui_root.to_path_buf();
        let result = std::thread::spawn(move || -> Result<(), CoreError> {
            let mut repo = git_ops::open_repo(&root3)?;
            git_ops::force_checkout(&mut repo, &latest)
        })
        .join();
        match result {
            Ok(Ok(())) => {
                tracing::warn!(
                    target = %latest_log,
                    "rollback completed (tier-2: latest_stable from LogStore cache); original_tag was lost"
                );
                return Ok(());
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %e, target = %latest_log, "rollback tier-2 failed, trying reset HEAD~1");
            }
            Err(e) => {
                tracing::warn!(?e, target = %latest_log, "rollback tier-2 thread panicked");
            }
        }
    } else {
        tracing::warn!("no latest_stable in LogStore cache, skipping tier-2 rollback");
    }

    // 兜底 3：执行 `git reset --hard HEAD~1`，至少回退一步
    let root4 = comfyui_root.to_path_buf();
    let result = std::thread::spawn(move || -> Result<(), CoreError> {
        // 第一阶段：拿到 parent_oid（Oid 是 Copy，不持有 repo 的 borrow）
        let parent_oid: git2::Oid = {
            let repo = git_ops::open_repo(&root4).map_err(|e| CoreError::GitError(e.to_string()))?;
            let head = repo.head().map_err(|e| CoreError::GitError(e.to_string()))?;
            let target = head.target().ok_or_else(|| {
                CoreError::GitError("HEAD is unborn, no target".to_string())
            })?;
            let commit = repo.find_commit(target).map_err(|e| CoreError::GitError(e.to_string()))?;
            // 直接返回 Oid（Copy），不带 commit 借用出 block
            commit.parent(0).map_err(|e| {
                CoreError::GitError(format!("HEAD is root commit, no parent: {}", e))
            }).map(|c| c.id())?
        };
        // 第二阶段：fresh repo，执行 set_head_detached + force checkout
        let repo = git_ops::open_repo(&root4).map_err(|e| CoreError::GitError(e.to_string()))?;
        let mut checkout_opts = git2::build::CheckoutBuilder::new();
        checkout_opts.force();
        // set_head_detached + checkout_head(force) 等价于 `git reset --hard HEAD~1`
        repo.set_head_detached(parent_oid).map_err(|e| CoreError::GitError(e.to_string()))?;
        repo.checkout_head(Some(&mut checkout_opts)).map_err(|e| CoreError::GitError(e.to_string()))?;
        Ok(())
    })
    .join();
    match result {
        Ok(Ok(())) => {
            tracing::warn!("rollback completed (tier-3: reset --hard HEAD~1)");
            Ok(())
        }
        Ok(Err(e)) => {
            tracing::error!(error = %e, "rollback tier-3 (reset HEAD~1) failed");
            // F35-E：tier-3 也失败的绝对兜底 —— 强制清 index 让 working tree 与 HEAD 一致
            // 关键：哪怕 HEAD 已经在孤儿 commit / detached 状态，至少 index 干净，
            //       下次切换按钮不会永久灰
            match emergency_reset_to_head(comfyui_root) {
                Ok(()) => {
                    tracing::warn!(
                        "rollback tier-3 failed but emergency_reset_to_head succeeded; \
                         HEAD may be in unexpected state but index is clean"
                    );
                    Ok(())
                }
                Err(reset_err) => {
                    Err(CoreError::GitError(format!(
                        "回滚彻底失败：original_tag / latest_stable / reset HEAD~1 / emergency_reset 四级兜底均失败。\
                         tier-3 错误: {}；emergency_reset 错误: {}。\
                         请手动在 ComfyUI 目录执行:\
                         \n  1. git status           # 看状态\
                         \n  2. git log --oneline -5  # 看历史\
                         \n  3. git reset HEAD        # 清 index (撤 staged)\
                         \n  4. git checkout <已知稳定版>  # 切到稳定版",
                        e, reset_err
                    )))
                }
            }
        }
        Err(e) => {
            tracing::error!(?e, "rollback tier-3 thread panicked");
            Err(CoreError::GitError(format!(
                "回滚 thread 异常: {:?}。请手动在 ComfyUI 目录执行 git status / git log / git checkout <已知稳定版>",
                e
            )))
        }
    }
}

/// F35-E：tier-3 失败后的绝对兜底
///
/// 独立线程跑 git2 操作（同步 C 库不能阻塞 tokio）。
/// 失败时回返 CoreError，但调用方应忽略错误（仅尽力兜底）。
fn emergency_reset_to_head(comfyui_root: &std::path::Path) -> Result<(), CoreError> {
    let root = comfyui_root.to_path_buf();
    std::thread::spawn(move || -> Result<(), CoreError> {
        let repo = git_ops::open_repo(&root).map_err(|e| CoreError::GitError(e.to_string()))?;
        git_ops::emergency_reset_to_head(&repo)
    })
    .join()
    .map_err(|e| CoreError::GitError(format!("emergency_reset thread join failed: {:?}", e)))?
}

/// 恢复 models 软链接（不抛错，仅记录日志）
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
        };
        assert_eq!(p.target_tag, "v0.3.10");
    }
}
