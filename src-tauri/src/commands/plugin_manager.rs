//! PluginManager 的 Tauri commands
//!
//! 设计模式：门面（Facade）- 前端仅与本层交互，不直接访问 Service
//!
//! 详见 `PR/03-模块设计/04-PluginManager.md §3 接口签名`

use tauri::{AppHandle, Emitter, State};

use crate::app_state::AppState;
use crate::plugin_manager::{
    PluginInfo, PluginListResult, PluginProgress, PluginUpdateInfo, UninstallResult, UpdateResult,
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

/// 安装插件（git clone）
///
/// - 仅支持 `https://` 协议
/// - 已存在则返回 `AlreadyExists`
/// - 克隆成功后自动尝试安装 requirements.txt（失败不影响插件本身可用）
/// - 进度通过 `plugin_progress` 事件推送到前端
#[tauri::command]
pub async fn plugin_install(
    url: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<PluginInfo, String> {
    // 构造进度回调（通过事件推送到前端）
    let progress = move |p: PluginProgress| {
        if let Err(e) = app.emit("plugin_progress", &p) {
            tracing::warn!(error = %e, "failed to emit plugin_progress");
        }
    };

    state.plugin_manager.install(&url, progress).await.map_err(|e| {
        tracing::error!(error = %e, url = %url, "plugin_install failed");
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
#[tauri::command]
pub async fn plugin_install_requirements(
    name: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .plugin_manager
        .install_requirements(&name)
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
