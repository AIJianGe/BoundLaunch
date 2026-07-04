//! LogStore Tauri commands（门面层）
//!
//! 设计模式：**门面 (Facade)** - 前端只与本层交互，不直接调 LogStoreService
//!
//! 详见 `PR/03-模块设计/09-LogStore.md §3 接口签名` 末尾的 `#[tauri::command]` 定义

use crate::app_state::AppState;
use crate::log_store::repository::{LogEntry, LogQueryFilter, TaskHistoryRecord};
use tauri::State;

/// 按条件查询历史日志
///
/// 前端示例：
/// ```ts
/// await invoke('log_query', {
///   filter: { level: 'error', limit: 100, offset: 0 }
/// })
/// ```
#[tauri::command]
pub async fn log_query(
    filter: LogQueryFilter,
    state: State<'_, AppState>,
) -> Result<Vec<LogEntry>, String> {
    state.log_store.logs().query(&filter).await.map_err(|e| e.to_string())
}

/// 取最近 N 行日志
///
/// 前端示例：`await invoke('log_tail', { lines: 500 })`
#[tauri::command]
pub async fn log_tail(
    lines: usize,
    state: State<'_, AppState>,
) -> Result<Vec<LogEntry>, String> {
    state.log_store.logs().tail(lines).await.map_err(|e| e.to_string())
}

/// 清空所有日志（调试 / 用户手动清理场景）
#[tauri::command]
pub async fn log_clear(state: State<'_, AppState>) -> Result<(), String> {
    state.log_store.logs().clear_all().await.map_err(|e| e.to_string())
}

/// 查询任务历史（前端「任务进度中心」页）
///
/// 前端示例：`await invoke('task_history_list', { limit: 100 })`
#[tauri::command]
pub async fn task_history_list(
    limit: usize,
    state: State<'_, AppState>,
) -> Result<Vec<TaskHistoryRecord>, String> {
    state.log_store.tasks().query_history(limit).await.map_err(|e| e.to_string())
}
