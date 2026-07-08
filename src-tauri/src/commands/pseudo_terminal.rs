//! 伪终端相关 Tauri 命令

use tauri::{AppHandle, State};

use crate::app_state::AppState;
use crate::pseudo_terminal::models::{PtySize, TerminalSessionInfo};

/// 创建终端会话
#[tauri::command]
pub async fn pty_create_session(
    app: AppHandle,
    state: State<'_, AppState>,
    shell: Option<String>,
    cwd: Option<String>,
    size: Option<PtySize>,
) -> Result<TerminalSessionInfo, String> {
    state
        .pseudo_terminal
        .create_session(app, shell, cwd, size)
        .await
}

/// 向终端写入数据（data 为 base64 编码的原始字节）
#[tauri::command]
pub async fn pty_write(
    state: State<'_, AppState>,
    session_id: String,
    data: String,
) -> Result<(), String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&data)
        .map_err(|e| format!("invalid base64: {}", e))?;
    state
        .pseudo_terminal
        .write(&session_id, &bytes)
        .await
}

/// 调整终端大小
#[tauri::command]
pub async fn pty_resize(
    state: State<'_, AppState>,
    session_id: String,
    size: PtySize,
) -> Result<(), String> {
    state.pseudo_terminal.resize(&session_id, size).await
}

/// 关闭终端会话
#[tauri::command]
pub async fn pty_close(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    state.pseudo_terminal.close(&session_id).await
}

/// 列出所有终端会话
#[tauri::command]
pub async fn pty_list_sessions(
    state: State<'_, AppState>,
) -> Result<Vec<TerminalSessionInfo>, String> {
    Ok(state.pseudo_terminal.list_sessions())
}

/// 获取单个会话信息
#[tauri::command]
pub async fn pty_get_session(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Option<TerminalSessionInfo>, String> {
    Ok(state.pseudo_terminal.get_session(&session_id))
}
