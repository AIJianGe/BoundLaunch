//! 任务执行 runner
//!
//! 职责：在 spawn 上下文中执行 action 闭包，
//! 处理 catch_unwind、cancel_token 检查，返回 FinalOutcome。
//!
//! 详见 `PR/03-模块设计/08-TaskScheduler.md §5.1 完整调度流程 §10.1 panic 恢复包装`

use std::panic::AssertUnwindSafe;

use futures::FutureExt;
use tauri::AppHandle;
use tokio_util::sync::CancellationToken;

use super::models::TaskId;
use super::progress::{spawn_flush_loop, ProgressSender};
use super::task::{FinalErr, FinalOutcome, TaskDef};

/// 执行单个任务（spawn 内部调用）
///
/// 流程：
/// 1. 创建 mpsc + ProgressSender
/// 2. spawn 后台 flush task（100ms 聚合 emit 到前端）
/// 3. catch_unwind(action(cancel_token, sender)).await
/// 4. 根据结果：
///    - Ok(Ok)    → Ok(TaskResult)
///    - Ok(Err)   → 若同时被取消则 Cancelled，否则 ActionFailed(msg)
///    - Err(panic) → Panicked(msg)
/// 5. flush task 在 sender drop 后自动退出（emit 最后一帧）
///
/// 注意：状态转换（Queued→Running→终态）、emit terminal、LogStore 写入由调用方处理
///
/// `app = None` 时（测试场景）跳过 emit，仅记录日志
pub(crate) async fn run_action(
    app: Option<AppHandle>,
    task_id: TaskId,
    def: TaskDef,
    cancel_token: CancellationToken,
) -> FinalOutcome {
    // 1. 创建 mpsc + ProgressSender
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let sender = ProgressSender { tx };

    // 2. spawn 后台 flush task（rx 在 sender drop 后自然关闭，flush 最后一帧并退出）
    let _flush_handle = spawn_flush_loop(app, task_id.clone(), rx);

    // 3. catch_unwind 包装 action
    let action_fut = (def.action)(cancel_token.clone(), sender);
    let result = AssertUnwindSafe(action_fut).catch_unwind().await;

    // 4. 根据结果转换（_flush_handle drop 时 flush task 会在 rx None 后退出，无需显式 await）
    match result {
        Ok(Ok(task_result)) => {
            tracing::info!(?task_id, summary = ?task_result.summary, "task action completed");
            Ok(task_result)
        }
        Ok(Err(msg)) => {
            // 同时被取消则视为 Cancelled（取消优先于业务 Err）
            if cancel_token.is_cancelled() {
                tracing::info!(?task_id, "task cancelled (action returned err after cancel)");
                Err(FinalErr::Cancelled)
            } else {
                tracing::warn!(?task_id, error = %msg, "task action returned error");
                Err(FinalErr::ActionFailed(msg))
            }
        }
        Err(panic) => {
            let msg = if let Some(s) = panic.downcast_ref::<&str>() {
                format!("panic: {}", s)
            } else if let Some(s) = panic.downcast_ref::<String>() {
                format!("panic: {}", s)
            } else {
                "panic: unknown".to_string()
            };
            tracing::error!(?task_id, error = %msg, "task action panicked, recovered");
            Err(FinalErr::Panicked(msg))
        }
    }
}
