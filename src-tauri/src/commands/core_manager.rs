//! CoreManager Tauri commands（门面层）
//!
//! 详见 `PR/03-模块设计/03-CoreManager.md §3 接口签名` 末尾的 `#[tauri::command]` 定义

use tauri::State;

use crate::app_state::AppState;
use crate::core_manager::compat::{
    detect_current_torch_variant, diff_requirements, parse_requirements_simple,
    recommend_mode, RequirementsDiff, SwitchMode, VersionCompatReport,
};
use crate::core_manager::models::{
    BackupInfo, CheckoutResult, ClassifiedTags, CoreStatus, SwitchPrerequisites, SwitchRepoResult,
    TagInfo,
};
use crate::core_manager::switcher::{build_switch_task, SwitchContext, SwitchVersionParams};
use crate::task_scheduler::TaskId;

/// 克隆 ComfyUI 仓库
#[tauri::command]
pub async fn core_clone(
    url: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let url = url.unwrap_or_else(|| crate::core_manager::models::COMFYUI_REPO_URL.to_string());
    state
        .core_manager
        .clone_repo(&url)
        .await
        .map_err(|e| e.to_string())
}

/// 确保 ComfyUI 仓库已克隆
///
/// - 若 `comfyui_root` 目录不存在或不含 `.git` → 自动 clone 默认仓库
/// - 若已是 git 仓库 → 跳过
/// - 若目录存在但非空且无 `.git` → 返回错误（让前端提示用户处理）
#[tauri::command]
pub async fn core_ensure_cloned(state: State<'_, AppState>) -> Result<(), String> {
    state
        .core_manager
        .ensure_cloned()
        .await
        .map_err(|e| e.to_string())
}

/// 列出所有 tag（force_refresh=true 强制刷新）
#[tauri::command]
pub async fn core_list_tags(
    force: bool,
    state: State<'_, AppState>,
) -> Result<Vec<TagInfo>, String> {
    state
        .core_manager
        .list_tags(force)
        .await
        .map_err(|e| e.to_string())
}

/// 列出所有 tag 并按 SemVer 分类（v3.1 / F26 决策 7：NTab 双分类）
///
/// 返回 ClassifiedTags：
/// - stable：稳定版（严格 vX.Y.Z）
/// - prerelease：预发布版（vX.Y.Z-rc1 / -beta 等）
#[tauri::command]
pub async fn core_list_tags_classified(
    force: bool,
    state: State<'_, AppState>,
) -> Result<ClassifiedTags, String> {
    state
        .core_manager
        .list_classified_tags(force)
        .await
        .map_err(|e| e.to_string())
}

/// 检查切换版本的前置条件（v3.1 / F26 决策 5）
///
/// 前端调用此命令判断是否允许切换：
/// - ComfyUI 运行中 → 拒绝
/// - 工作区有未提交改动 → 拒绝
#[tauri::command]
pub async fn core_check_switch_prerequisites(
    state: State<'_, AppState>,
) -> Result<SwitchPrerequisites, String> {
    let comfyui_running = state.process_launcher.status().await.is_alive();
    state
        .core_manager
        .check_switch_prerequisites(comfyui_running)
        .await
        .map_err(|e| e.to_string())
}

/// 切换 ComfyUI 版本（v3.1 / F26 决策 1-12 完整实现）
///
/// 行为（11 步流程 + 全部回滚）：
/// 1. 前置检查：ComfyUI 已停止 + 工作区干净
/// 2. 记录当前 tag（用于回滚）
/// 3. 解除 models 软链接（避免 git checkout 冲突）
/// 4. fetch 远程 tag（决策 10：本地优先）
/// 5. 检查目标 tag 是否存在
/// 6. checkout 到目标 tag
/// 7. 重新建立 models 软链接
/// 8. 依 mode 处理 venv：
///    - Clean：删 venv → 重建 → 装 requirements + torch
///    - Preserve：保留 venv → pip install -r new-req.txt --upgrade --force-reinstall
///    - Skip：不动 venv
/// 9. 验证 venv 完整性
///
/// 失败时全部回滚（决策 6）：force_checkout 回原 tag + 恢复 models 链接。
/// venv 状态可能不一致（已删除但未重建成功），由用户在 UI 重新初始化。
///
/// 返回 task_id，前端通过 listen('task_progress'/'task_completed') 接收进度。
///
/// v1.8 / F36 新增 `mode` 参数：用户在对话框选择的切换模式
#[tauri::command]
pub async fn core_switch_version(
    target_tag: String,
    mode: Option<String>,
    state: State<'_, AppState>,
) -> Result<TaskId, String> {
    // 解析 mode 字符串为 enum
    let mode = match mode.as_deref() {
        Some("Clean") | Some("clean") => SwitchMode::Clean,
        Some("Skip") | Some("skip") => SwitchMode::Skip,
        _ => SwitchMode::Preserve, // 默认 Preserve（最常用）
    };
    let ctx = SwitchContext {
        config: state.config.clone(),
        event_bus: state.event_bus.clone(),
        log_store: state.log_store.clone(),
        python_env: state.python_env.clone(),
        process_launcher: state.process_launcher.clone(),
    };
    let params = SwitchVersionParams { target_tag, mode };
    let def = build_switch_task(params, ctx);
    state
        .task_scheduler
        .submit(def)
        .await
        .map_err(|e| e.to_string())
}

/// v1.8 / F36：版本切换兼容性预检
///
/// 切版本前前端调用，弹对话框显示该报告
#[tauri::command]
pub async fn core_check_version_compatibility(
    target_tag: String,
    state: State<'_, AppState>,
) -> Result<VersionCompatReport, String> {
    let config = state.config.get();
    let comfyui_root = config.paths.comfyui_root.clone();
    let venv_path = config.paths.venv_path.clone();
    let target_python_version = config.paths.python_version.clone();
    let target_torch_variant = config.torch.cuda_version.to_torch_index().to_string();

    // 1. 读 venv 状态
    let venv_exists = venv_path.join("pyvenv.cfg").exists();
    let current_python = if venv_exists {
        crate::python_env::verify::probe_python_version(
            &crate::python_env::uv_runner::venv_python_path(&venv_path),
        )
        .await
    } else {
        None
    };
    let current_torch_variant = detect_current_torch_variant(&venv_path).await;
    let current_torch_installed = current_torch_variant.is_some();

    // 2. 读当前 HEAD tag
    let current_tag = state.core_manager.current_version().await.ok().and_then(|s| s.current_version);

    // 3. 读两个 tag 的 requirements.txt 并 diff
    let read_reqs_for_tag = |tag: &str| -> Option<Vec<(String, String)>> {
        let req_path = comfyui_root.join("requirements.txt");
        // 当前 tag：从工作树读（HEAD 已 checkout）
        // target tag：从 git show tag:requirements.txt 读
        if tag == current_tag.as_deref().unwrap_or("") {
            std::fs::read_to_string(&req_path)
                .ok()
                .map(|c| parse_requirements_simple(&c))
        } else {
            // 通过 git show 读指定 tag 的 requirements.txt
            let output = std::process::Command::new("git")
                .args(["show", &format!("{}:requirements.txt", tag)])
                .current_dir(&comfyui_root)
                .output()
                .ok()?;
            if output.status.success() {
                let s = String::from_utf8_lossy(&output.stdout).to_string();
                Some(parse_requirements_simple(&s))
            } else {
                None
            }
        }
    };
    let current_reqs = current_tag
        .as_ref()
        .and_then(|t| read_reqs_for_tag(t))
        .unwrap_or_default();
    let target_reqs = read_reqs_for_tag(&target_tag).unwrap_or_default();
    let requirements_diff = diff_requirements(&current_reqs, &target_reqs);

    // 4. 算 same_python / same_torch_variant
    let same_python = match (&current_python, &target_python_version) {
        (Some(cp), tp) => cp.starts_with(tp),
        (None, _) => false, // venv 不存在 → 算不一致（必须 Clean）
    };
    let same_torch_variant = match (&current_torch_variant, &target_torch_variant) {
        (Some(ctv), ttv) => ctv == ttv,
        (None, _) => false,
    };

    // 5. custom_nodes 数量
    let custom_nodes_dir = comfyui_root.join("custom_nodes");
    let custom_node_count = std::fs::read_dir(&custom_nodes_dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().join(".git").exists() || e.path().is_dir())
                .count()
        })
        .unwrap_or(0);

    // 6. 推荐模式
    let (recommended_mode, recommended_reason) = recommend_mode(
        same_python,
        same_torch_variant,
        venv_exists,
        &requirements_diff,
        current_torch_installed,
    );

    Ok(VersionCompatReport {
        current_tag,
        target_tag,
        venv_exists,
        current_python,
        target_python: Some(target_python_version),
        current_torch_variant,
        target_torch_variant: Some(target_torch_variant),
        current_torch_installed,
        same_python,
        same_torch_variant,
        requirements_diff,
        custom_node_count,
        recommended_mode,
        recommended_reason,
    })
}

/// 切换到指定 tag
#[tauri::command]
pub async fn core_checkout(
    tag: String,
    state: State<'_, AppState>,
) -> Result<CheckoutResult, String> {
    state
        .core_manager
        .checkout(&tag)
        .await
        .map_err(|e| e.to_string())
}

/// 更新到最新稳定版
#[tauri::command]
pub async fn core_update(state: State<'_, AppState>) -> Result<String, String> {
    state
        .core_manager
        .update_latest_stable()
        .await
        .map_err(|e| e.to_string())
}

/// 查询当前仓库状态
#[tauri::command]
pub async fn core_status(state: State<'_, AppState>) -> Result<CoreStatus, String> {
    state
        .core_manager
        .current_version()
        .await
        .map_err(|e| e.to_string())
}

/// 检查仓库是否已克隆
#[tauri::command]
pub async fn core_is_cloned(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(state.core_manager.is_cloned().await)
}

/// 确保 models 软链接已建立（v3.1 / F26 决策 12）
///
/// 在以下场景调用：
/// - 用户在设置页配置/修改 models_path 后
/// - ComfyUI 启动前（ProcessLauncher 内部调用）
/// - 切换版本任务步骤 7（switcher 内部调用）
///
/// 行为：
/// - models_path = None → 跳过（用默认 `<comfyui_root>/models`）
/// - models_path = Some(p) → 建立 `<comfyui_root>/models` 软链接到 p
#[tauri::command]
pub async fn core_ensure_models_link(state: State<'_, AppState>) -> Result<bool, String> {
    let comfyui_root: std::path::PathBuf;
    let models_path: Option<std::path::PathBuf>;
    {
        let config = state.config.get();
        comfyui_root = config.paths.comfyui_root.clone();
        models_path = config.paths.models_path.clone();
    }

    crate::core_manager::paths::ensure_models_link(&comfyui_root, models_path.as_deref())
        .map(|opt| opt.is_some())
        .map_err(|e| e.to_string())
}

// ============================================================================
// F31：仓库地址切换与备份恢复
// ============================================================================

/// 获取当前仓库 URL（脱敏后的，用于前端显示）
#[tauri::command]
pub async fn core_get_repo_url(state: State<'_, AppState>) -> Result<String, String> {
    Ok(state.core_manager.get_repo_url_masked())
}

/// 获取官方仓库 URL（常量，供前端「恢复官方」按钮使用）
#[tauri::command]
pub async fn core_official_repo_url(
    state: State<'_, AppState>,
) -> Result<String, String> {
    Ok(state.core_manager.official_repo_url().to_string())
}

/// 列出所有备份
#[tauri::command]
pub async fn core_list_backups(state: State<'_, AppState>) -> Result<Vec<BackupInfo>, String> {
    state
        .core_manager
        .list_backups()
        .await
        .map_err(|e| e.to_string())
}

/// 切换仓库地址
///
/// 参数：
/// - `url`: 新仓库 URL（GitHub，支持带 token 的私有仓库）
/// - `migrate_custom_nodes`: 是否迁移 custom_nodes 到新仓库
///
/// 返回 SwitchRepoResult：
/// - `success`：切换成功（含备份名、耗时）
/// - `rolled_back`：切换失败但已回滚（含错误信息）
#[tauri::command]
pub async fn core_set_repo_url(
    url: String,
    migrate_custom_nodes: bool,
    state: State<'_, AppState>,
) -> Result<SwitchRepoResult, String> {
    state
        .core_manager
        .switch_repo_url(&url, migrate_custom_nodes)
        .await
        .map_err(|e| e.to_string())
}

/// 恢复备份
///
/// 参数：
/// - `backup_name`: 备份目录名（如 "ComfyUI_bak01"）
///
/// 返回 SwitchRepoResult（同 switch_repo_url）
#[tauri::command]
pub async fn core_restore_backup(
    backup_name: String,
    state: State<'_, AppState>,
) -> Result<SwitchRepoResult, String> {
    state
        .core_manager
        .restore_backup(&backup_name)
        .await
        .map_err(|e| e.to_string())
}

/// F35-D：在系统文件管理器中打开 ComfyUI 仓库目录
///
/// 用途：用户工作区脏时，UI 提供"打开 ComfyUI 目录"按钮让用户手动 `git stash / git clean`
///
/// 跨平台：
/// - Windows: `cmd /c start "" "<path>"`（start 会先转 canonical path 再调 explorer，
///   避免直接 `explorer.exe <path>` 的参数解析 bug——当路径无 trailing `\` 时 explorer
///   会回退到"文档"库等虚拟位置）
/// - macOS: `open <path>`
/// - Linux: `xdg-open <path>`
///
/// 安全：仅打开 comfyui_root（来自 ConfigService 已校验的路径），不接收用户输入路径，
/// 避免命令注入风险
#[tauri::command]
pub async fn core_open_comfyui_dir(state: State<'_, AppState>) -> Result<(), String> {
    let config = state.config.get();
    let comfyui_root = config.paths.comfyui_root.clone();
    if !comfyui_root.exists() {
        return Err(format!("ComfyUI 目录不存在: {}", comfyui_root.display()));
    }
    let result = if cfg!(target_os = "windows") {
        // cmd /c start 第一个 "" 是窗口标题（必需），第二个 "" 包裹路径（允许空格）
        let path_str = comfyui_root.display().to_string();
        std::process::Command::new("cmd")
            .args(["/c", "start", "", &path_str])
            .spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(&comfyui_root).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(&comfyui_root).spawn()
    };
    result.map(|_| ()).map_err(|e| {
        format!(
            "无法打开目录 {}: {}",
            comfyui_root.display(),
            e
        )
    })
}

/// F35-A+：返回工作区脏的原因（staged / unstaged / untracked）
///
/// 用例：前端 `coreStatus()` 拿到 `has_local_changes=true` 后，调用本命令
/// 获取详细原因，给用户针对性提示。
///
/// 返回：None = 干净；Some(reason) = 详细原因（最多前 20 个文件路径）
#[tauri::command]
pub async fn core_workspace_dirty_reason(
    state: State<'_, AppState>,
) -> Result<Option<crate::core_manager::git_ops::WorkspaceDirtyReason>, String> {
    let comfyui_root = state.config.get().paths.comfyui_root.clone();
    if !comfyui_root.join(".git").exists() {
        return Ok(None);
    }
    let reason = tokio::task::spawn_blocking(move || -> Result<Option<crate::core_manager::git_ops::WorkspaceDirtyReason>, String> {
        let repo = crate::core_manager::git_ops::open_repo(&comfyui_root)
            .map_err(|e| e.to_string())?;
        Ok(crate::core_manager::git_ops::inspect_workspace_dirty(&repo))
    })
    .await
    .map_err(|e| format!("检查工作区状态失败: {}", e))??;
    Ok(reason)
}

/// F35-A+：撤销 staging（`git reset HEAD`），不修改 working tree 内容
///
/// ⚠️ 仅撤销 staging，**不删除**任何文件内容。staged 改动会回到 working tree。
/// unstaged 改动和 untracked 文件**完全不受影响**。
///
/// 用例：用户有 staged 改动但想撤销 staging（如放弃切版本时的中间状态）。
#[tauri::command]
pub async fn core_reset_staged(state: State<'_, AppState>) -> Result<(), String> {
    let comfyui_root = state.config.get().paths.comfyui_root.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let repo = crate::core_manager::git_ops::open_repo(&comfyui_root)
            .map_err(|e| e.to_string())?;
        crate::core_manager::git_ops::reset_staged(&repo).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("reset_staged 失败: {}", e))??;
    Ok(())
}

/// F35-A+：强制清理整个工作区（`git checkout .` + `git clean -fd`）
///
/// ⚠️ **不可恢复**：会丢弃所有 tracked 改动和 untracked 文件。
/// 前端需弹确认对话框，用户明确同意后调用。
#[tauri::command]
pub async fn core_force_clean_workspace(state: State<'_, AppState>) -> Result<(), String> {
    let comfyui_root = state.config.get().paths.comfyui_root.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let repo = crate::core_manager::git_ops::open_repo(&comfyui_root)
            .map_err(|e| e.to_string())?;
        crate::core_manager::git_ops::force_clean_workspace(&repo, &comfyui_root)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("force_clean_workspace 失败: {}", e))??;
    Ok(())
}
