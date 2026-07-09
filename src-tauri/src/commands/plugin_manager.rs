//! PluginManager 的 Tauri commands
//!
//! 设计模式：门面（Facade）- 前端仅与本层交互，不直接访问 Service
//!
//! 详见 `PR/03-模块设计/04-PluginManager.md §3 接口签名`

use tauri::{AppHandle, Emitter, State};

use crate::app_state::AppState;
use crate::plugin_manager::{
    venv_health::{check_venv_health, clean_broken_distributions, VenvHealthReport},
    LocalRefInfo, PluginInfo, PluginListResult, PluginProgress, PluginUpdateInfo, RemoteTagInfo,
    SwitchResult, UninstallResult, UpdateResult,
};

/// 列出所有已安装插件（30s 缓存）
///
/// `force=true` 强制刷新缓存
#[tauri::command]
pub async fn plugin_list(
    force: bool,
    state: State<'_, AppState>,
) -> Result<PluginListResult, String> {
    state.plugin_manager.list_plugins(force).await.map_err(|e| {
        tracing::error!(error = %e, "plugin_list failed");
        e.to_string()
    })
}

/// 安装插件（git clone，可选 checkout 到指定 tag）
///
/// - 仅支持 `https://` 协议
/// - 已存在则返回 `AlreadyExists`
/// - `tag` 非 None 时 clone 后 checkout 到该 tag（detached HEAD）
/// - 克隆成功后自动尝试安装 requirements.txt（失败不影响插件本身可用）
/// - 进度通过 `plugin_progress` 事件推送到前端
#[tauri::command]
pub async fn plugin_install(
    url: String,
    tag: Option<String>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<PluginInfo, String> {
    // 构造进度回调（通过事件推送到前端）
    let progress = move |p: PluginProgress| {
        if let Err(e) = app.emit("plugin_progress", &p) {
            tracing::warn!(error = %e, "failed to emit plugin_progress");
        }
    };

    state
        .plugin_manager
        .install(&url, tag.as_deref(), progress)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, url = %url, "plugin_install failed");
            e.to_string()
        })
}

/// 列出远程仓库的 tag（不下载整个仓库，仅 ls-remote）
///
/// 用于安装前让用户选择 tag 版本。返回按名称降序排列。
#[tauri::command]
pub async fn plugin_list_remote_tags(
    url: String,
    state: State<'_, AppState>,
) -> Result<Vec<RemoteTagInfo>, String> {
    state
        .plugin_manager
        .list_remote_tags(&url)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, url = %url, "plugin_list_remote_tags failed");
            e.to_string()
        })
}

/// 更新插件（git pull）
#[tauri::command]
pub async fn plugin_update(
    name: String,
    state: State<'_, AppState>,
) -> Result<UpdateResult, String> {
    state.plugin_manager.update(&name).await.map_err(|e| {
        tracing::error!(error = %e, %name, "plugin_update failed");
        e.to_string()
    })
}

/// 卸载插件（移到回收站）
///
/// - 不存在返回 `NotFound`
/// - 成功后返回 `UninstallResult`（含回收站路径）
#[tauri::command]
pub async fn plugin_uninstall(
    name: String,
    state: State<'_, AppState>,
) -> Result<UninstallResult, String> {
    state.plugin_manager.uninstall(&name).await.map_err(|e| {
        tracing::error!(error = %e, %name, "plugin_uninstall failed");
        e.to_string()
    })
}

/// 启用/禁用插件
///
/// 幂等：当前状态等于目标状态时不操作。
#[tauri::command]
pub async fn plugin_toggle(
    name: String,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.plugin_manager.toggle(&name, enabled).await.map_err(|e| {
        tracing::error!(error = %e, %name, enabled, "plugin_toggle failed");
        e.to_string()
    })
}

/// 安装插件的 requirements.txt
///
/// - 无 requirements.txt → Ok（视为已满足）
/// - venv 未就绪 → VenvNotReady
/// - pip install 失败 → RequirementsFailed
/// - `force_reinstall` 为 true 时加 `--force-reinstall`（切版本时用）
#[tauri::command]
pub async fn plugin_install_requirements(
    name: String,
    force_reinstall: Option<bool>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .plugin_manager
        .install_requirements(&name, force_reinstall.unwrap_or(false))
        .await
        .map_err(|e| {
            tracing::error!(error = %e, %name, "plugin_install_requirements failed");
            e.to_string()
        })
}

/// 检查所有插件的远程更新
#[tauri::command]
pub async fn plugin_check_updates(
    state: State<'_, AppState>,
) -> Result<Vec<PluginUpdateInfo>, String> {
    state.plugin_manager.check_updates().await.map_err(|e| {
        tracing::error!(error = %e, "plugin_check_updates failed");
        e.to_string()
    })
}

/// 获取单个插件信息
#[tauri::command]
pub async fn plugin_info(
    name: String,
    state: State<'_, AppState>,
) -> Result<PluginInfo, String> {
    state.plugin_manager.get_plugin_info(&name).await.map_err(|e| {
        tracing::error!(error = %e, %name, "plugin_info failed");
        e.to_string()
    })
}

// ============ v3.x：版本切换相关命令 ============

/// 列出指定插件的可用 ref（本地 tag + branch）
///
/// **调用时机**：用户点"切版本"按钮时。
/// **性能**：会先 `git fetch --tags`（1-5s），结果不缓存。
#[tauri::command]
pub async fn plugin_list_available_versions(
    name: String,
    state: State<'_, AppState>,
) -> Result<Vec<LocalRefInfo>, String> {
    state
        .plugin_manager
        .list_available_versions(&name)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, %name, "plugin_list_available_versions failed");
            e.to_string()
        })
}

/// 切换插件到指定 ref（tag / branch / commit hash）
///
/// **完整流程**：
/// 1. fetch 最新 tag
/// 2. checkout_ref（detached HEAD，记录 previous_commit 到 .launcher_backup_commit）
/// 3. 装依赖（force_reinstall=true）— 失败不阻塞
/// 4. 返回 SwitchResult（plugin + previous_commit + need_restart）
///
/// **进度**：通过 `plugin_progress` 事件推送 Switching{0..100} + InstallingRequirements{0..100}。
#[tauri::command]
pub async fn plugin_switch_version(
    name: String,
    target_ref: String,
    state: State<'_, AppState>,
) -> Result<SwitchResult, String> {
    state
        .plugin_manager
        .switch_version(&name, &target_ref)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, %name, target = %target_ref, "plugin_switch_version failed");
            e.to_string()
        })
}

/// 回滚到上次切版本前的 commit
///
/// 需要 plugin 目录中有 `.launcher_backup_commit` 文件。
#[tauri::command]
pub async fn plugin_rollback_version(
    name: String,
    state: State<'_, AppState>,
) -> Result<PluginInfo, String> {
    state
        .plugin_manager
        .rollback_version(&name)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, %name, "plugin_rollback_version failed");
            e.to_string()
        })
}

// ============ v3.x：venv 健康检查命令 ============

/// venv 健康检查（检测损坏包 + 验证关键 import）
///
/// **返回**：完整报告（损坏包列表 + import 检查结果 + 状态枚举）
///
/// **典型调用时机**：
/// - 插件页打开时自动跑一次
/// - 装依赖失败时用户点"诊断"
/// - 用户主动点"修复 venv"前的检测
#[tauri::command]
pub async fn plugin_health_check_venv(
    state: State<'_, AppState>,
) -> Result<VenvHealthReport, String> {
    let venv_path = state.plugin_manager.current_venv_path();
    tracing::info!(?venv_path, "plugin_health_check_venv started");
    let report = check_venv_health(&venv_path).await;
    tracing::info!(
        status = ?report.status,
        broken = report.broken_distributions.len(),
        imports_failed = report.critical_imports.iter().filter(|i| !i.ok).count(),
        elapsed_ms = report.elapsed_ms,
        "plugin_health_check_venv done"
    );
    Ok(report)
}

/// 一键修复 venv（清理损坏包 + 重新跑 import 验证）
///
/// **完整流程**：
/// 1. 删 `~xxx*` 损坏目录
/// 2. 重新跑 health check
/// 3. 如果 import 还是失败，**返回报告但不自动重装**（避免误操作）
///
/// **返回**：清理后的健康报告（前端根据 status 决定是否提示用户重装）
#[tauri::command]
pub async fn plugin_fix_venv(
    state: State<'_, AppState>,
) -> Result<VenvHealthReport, String> {
    let venv_path = state.plugin_manager.current_venv_path();
    let site_packages = crate::plugin_manager::venv_health::site_packages_path(&venv_path);

    tracing::info!(?site_packages, "plugin_fix_venv started");

    // 1. 清理损坏包
    if site_packages.exists() {
        match clean_broken_distributions(&site_packages) {
            Ok(removed) => {
                tracing::info!(count = removed.len(), "cleaned broken distributions");
            }
            Err(e) => {
                tracing::warn!(error = %e, "clean_broken_distributions failed");
            }
        }
    }

    // 2. 重新健康检查
    let report = check_venv_health(&venv_path).await;
    tracing::info!(
        status = ?report.status,
        "plugin_fix_venv done"
    );
    Ok(report)
}

// ============ v3.x：ComfyUI 核心依赖管理命令 ============

use crate::plugin_manager::comfyui_core::{
    ComfyUICoreRequirementsStatus, PreLaunchCheck,
};

/// v3.x：检查 ComfyUI 核心依赖状态
///
/// **检测原理**：用 SHA256 算 ComfyUI/requirements.txt 的内容指纹
/// 与状态文件 `.comfyui_requirements_hash` 中上次成功装时的 hash 对比。
///
/// **典型调用时机**：
/// - 用户点"启动 ComfyUI"前自动调一次
/// - 用户在 PluginPage 主动点"装核心依赖"前的检测
/// - 进 ComfyUI 设置页时静默预检
#[tauri::command]
pub async fn plugin_check_comfyui_requirements(
    force_reinstall: bool,
    state: State<'_, AppState>,
) -> Result<ComfyUICoreRequirementsStatus, String> {
    let status = state.plugin_manager.check_comfyui_requirements(force_reinstall);
    tracing::info!(
        needs_install = status.needs_install,
        reason = %status.reason,
        elapsed_ms = status.elapsed_ms,
        "plugin_check_comfyui_requirements"
    );
    Ok(status)
}

/// v3.x：启动 ComfyUI 前的完整检查
///
/// **检查 2 件事**：
/// 1. ComfyUI 核心 requirements 是否需要装（hash 变了？）
/// 2. 所有 enabled 插件中 requirements.txt 存在但未装的
///
/// **返回**：`PreLaunchCheck { core_requirements, plugins_needing_install, all_ok }`
///
/// **典型调用时机**：
/// - 用户点"启动 ComfyUI"前自动调一次
/// - 根据返回结果显示"启动前是否需要装依赖"弹窗
#[tauri::command]
pub async fn plugin_launch_pre_check(
    force_reinstall: bool,
    state: State<'_, AppState>,
) -> Result<PreLaunchCheck, String> {
    let check = state.plugin_manager.launch_pre_check(force_reinstall);
    tracing::info!(
        core_needs = check.core_requirements.needs_install,
        plugins_needs = check.plugins_needing_install.len(),
        all_ok = check.all_ok,
        elapsed_ms = check.elapsed_ms,
        "plugin_launch_pre_check"
    );
    Ok(check)
}

/// v3.x：装 ComfyUI 核心依赖
///
/// **完整流程**：
/// 1. 前置清理 site-packages 损坏包（~xxx*）
/// 2. emit `InstallingRequirements { 0 }` 事件（让前端打开进度面板）
/// 3. `pip install -r ComfyUI/requirements.txt`（可选 `--force-reinstall`）
/// 4. 实时 emit `plugin_progress_log` 事件 + 写 LogStore
/// 5. 装完验证关键 import（safetensors.torch / folder_paths / comfy.samplers 等）
/// 6. 写状态文件 `<custom_nodes_parent>/.comfyui_requirements_hash`
/// 7. emit `InstallingRequirements { 100 }` + `Done` 事件
///
/// **返回**：装成功时返回 hash（hex 64 字符）
#[tauri::command]
pub async fn plugin_install_comfyui_requirements(
    force_reinstall: bool,
    state: State<'_, AppState>,
) -> Result<String, String> {
    tracing::info!(force_reinstall, "plugin_install_comfyui_requirements started");
    state
        .plugin_manager
        .install_comfyui_requirements(force_reinstall)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "plugin_install_comfyui_requirements failed");
            e.to_string()
        })
}
