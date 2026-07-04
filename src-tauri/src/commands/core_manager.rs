//! CoreManager Tauri commands（门面层）
//!
//! 详见 `PR/03-模块设计/03-CoreManager.md §3 接口签名` 末尾的 `#[tauri::command]` 定义

use tauri::State;

use crate::app_state::AppState;
use crate::core_manager::models::{CheckoutResult, CoreStatus, TagInfo};

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
