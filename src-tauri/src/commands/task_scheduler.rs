//! TaskScheduler 的 Tauri commands
//!
//! 设计模式：门面（Facade）- 前端仅与本层交互，不直接访问 Service
//!
//! 详见 `PR/03-模块设计/08-TaskScheduler.md §3 接口签名`
//!
//! 注意：submit 不暴露为 Tauri command，仅由后端各 Service 调用
//! （前端通过 listen('task_queued'/'task_progress'/'task_completed') 接收进度）

use tauri::State;

use crate::app_state::AppState;
use crate::task_scheduler::TaskInfo;

/// 列出所有任务快照（按 started_at 倒序）
#[tauri::command]
pub async fn task_list(state: State<'_, AppState>) -> Result<Vec<TaskInfo>, String> {
    Ok(state.task_scheduler.list().await)
}

/// 取消任务（已终态任务返回 Ok，幂等）
#[tauri::command]
pub async fn task_cancel(id: String, state: State<'_, AppState>) -> Result<(), String> {
    state.task_scheduler.cancel(&id).await.map_err(|e| {
        tracing::error!(error = %e, %id, "task_cancel failed");
        e.to_string()
    })
}

/// 查询单个任务
#[tauri::command]
pub async fn task_get(id: String, state: State<'_, AppState>) -> Result<Option<TaskInfo>, String> {
    Ok(state.task_scheduler.get(&id).await)
}
