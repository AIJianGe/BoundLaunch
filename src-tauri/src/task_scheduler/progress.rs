//! 进度推送聚合
//!
//! 设计模式：观察者（Observer）- 100ms 批量聚合后 emit 到前端
//!
//! 详见 `PR/03-模块设计/08-TaskScheduler.md §4.4 进度推送聚合`
//!
//! ## 关键约束
//! - 100ms 是人类可感知的最小延迟
//! - 再低则 IPC 压力陡增（前端 Vue 重渲染成本高），实测 60fps 渲染足够顺滑
//! - 单任务最多 10 次/秒 emit
//! - 测试环境无 AppHandle 时跳过 emit（仅记录日志）

use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;

use super::models::TaskId;

/// 进度消息（action 通过 ProgressSender 上报）
#[derive(Debug, Clone)]
pub(crate) enum ProgressMsg {
    /// 0..=100（超界会被 clamp）
    Update(u8),
    /// 可选的人类可读消息
    Message(String),
    /// v3.5 新增：实时日志行（子进程输出）
    ///
    /// 与 Message 不同：Log 行不聚合到「最新一条 message」，
    /// 而是累积到日志缓冲区，前端可一次性拿全部或订阅增量。
    Log {
        source: String,
        text: String,
    },
}

/// 进度推送句柄：action 内部调用 send_* 上报进度
///
/// Clone 后可在闭包内多处使用（mpsc::UnboundedSender 本身支持多 producer）
#[derive(Clone)]
pub struct ProgressSender {
    pub(crate) tx: mpsc::UnboundedSender<ProgressMsg>,
}

impl ProgressSender {
    /// 上报进度百分比（0..=100），超界会被 clamp 到 100
    pub fn send_percent(&self, percent: u8) {
        let _ = self.tx.send(ProgressMsg::Update(percent.min(100)));
    }

    /// 上报一条人类可读消息（如 "正在下载 torch-2.4.0-cp311..."）
    pub fn send_message(&self, msg: impl Into<String>) {
        let _ = self.tx.send(ProgressMsg::Message(msg.into()));
    }

    /// v3.5 新增：推送一行实时日志（子进程输出）
    ///
    /// 与 `send_message` 的区别：
    /// - `send_message` 是**聚合式**（100ms flush 取最新一条，覆盖式）
    /// - `send_log` 是**累积式**（每条独立发送，前端可订阅增量流）
    ///
    /// 用法：把子进程 stdout/stderr 的每一行调 `send_log(source, line)`，
    /// 前端 `useTaskProgress.onLog()` 会拿到所有行（带时间戳、来源），形成实时日志面板。
    ///
    /// source 用于前端分组显示（如 "git fetch" / "uv pip install" / "torch"）。
    pub fn send_log(&self, source: impl Into<String>, text: impl Into<String>) {
        let _ = self.tx.send(ProgressMsg::Log {
            source: source.into(),
            text: text.into(),
        });
    }

    /// 创建 no-op 进度发送器（v1.8 / F36）
    ///
    /// 用途：在非任务上下文（如启动时一次性迁移）调用 `recovery::quick_repair_*`，
    /// 这些函数需要 `&ProgressSender`，但不希望上报进度事件。
    /// no-op 实现：所有 send_* 调用都静默丢弃。
    pub fn no_op() -> Self {
        // 创建会立即 drop 的 channel，所有 send 都被静默丢弃
        let (tx, _rx) = mpsc::unbounded_channel();
        Self { tx }
    }
}

/// 推送给前端的进度事件 payload
#[derive(Debug, Clone, Serialize)]
pub struct ProgressEvent {
    pub task_id: TaskId,
    pub progress: u8,
    pub message: Option<String>,
    /// 当前状态字符串：queued / running / completed / failed / cancelled
    pub status: String,
}

/// 推送给前端的实时日志事件 payload
#[derive(Debug, Clone, Serialize)]
pub struct LogEvent {
    pub task_id: TaskId,
    pub source: String,
    pub text: String,
    /// ms since epoch，前端可直接显示
    pub ts_ms: u64,
}

/// 启动后台 flush task，聚合进度推送
///
/// - 每 100ms 唤醒一次，取最新 progress + 最新 message
/// - 关闭 rx（action 完成，sender 全部 drop）时 flush 最后一帧并退出
/// - `app = None` 时跳过 emit（测试场景），仅记录日志
/// - 任何 emit 失败仅 warn，不阻塞 action
///
/// v3.5 扩展：Log 消息**不聚合**，每条立即 emit（前端订阅日志流）。
/// - 但为防止 uv 大流量输出压垮 IPC，单任务 50ms 内最多 emit 1 个 log 批次。
pub(crate) fn spawn_flush_loop(
    app: Option<AppHandle>,
    task_id: TaskId,
    mut rx: mpsc::UnboundedReceiver<ProgressMsg>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let interval = Duration::from_millis(100);
        let mut latest_progress: u8 = 0;
        let mut latest_message: Option<String> = None;
        let mut dirty = false;
        // v3.5：日志批量 buffer（节流）
        let mut log_batch: Vec<(String, String)> = Vec::with_capacity(32);
        let mut last_log_emit = std::time::Instant::now();

        loop {
            tokio::select! {
                // 收到新消息（rx 关闭时返回 None → flush 最后一帧并退出）
                msg = rx.recv() => {
                    match msg {
                        Some(ProgressMsg::Update(p)) => {
                            latest_progress = p;
                            dirty = true;
                        }
                        Some(ProgressMsg::Message(m)) => {
                            latest_message = Some(m);
                            dirty = true;
                        }
                        Some(ProgressMsg::Log { source, text }) => {
                            // Log 消息累积到 batch，每 50ms emit 一次（节流）
                            log_batch.push((source, text));
                            if last_log_emit.elapsed() >= Duration::from_millis(50) && !log_batch.is_empty() {
                                emit_log_batch(&app, &task_id, &mut log_batch);
                                last_log_emit = std::time::Instant::now();
                            }
                        }
                        None => {
                            // sender 全部 drop，flush 最后一帧并退出
                            if dirty {
                                emit_progress(&app, &task_id, latest_progress, latest_message.clone(), "running");
                            }
                            if !log_batch.is_empty() {
                                emit_log_batch(&app, &task_id, &mut log_batch);
                            }
                            break;
                        }
                    }
                }
                // 100ms 定时 flush
                _ = tokio::time::sleep(interval) => {
                    if dirty {
                        emit_progress(&app, &task_id, latest_progress, latest_message.clone(), "running");
                        dirty = false;
                    }
                    if !log_batch.is_empty() && last_log_emit.elapsed() >= Duration::from_millis(50) {
                        emit_log_batch(&app, &task_id, &mut log_batch);
                        last_log_emit = std::time::Instant::now();
                    }
                }
            }
        }
    })
}

/// 批量 emit 日志（v3.5）
fn emit_log_batch(app: &Option<AppHandle>, task_id: &TaskId, batch: &mut Vec<(String, String)>) {
    if batch.is_empty() {
        return;
    }
    if let Some(app) = app {
        // 一次 emit 整批（payload 是 Vec<LogEvent>，前端追加到 UI）
        let now = chrono::Utc::now().timestamp_millis() as u64;
        let events: Vec<LogEvent> = batch
            .drain(..)
            .map(|(source, text)| LogEvent {
                task_id: task_id.clone(),
                source,
                text,
                ts_ms: now,
            })
            .collect();
        if let Err(e) = app.emit("task_log", &events) {
            tracing::warn!(?task_id, error = %e, "failed to emit task_log");
        }
    } else {
        // 测试场景：仅记录日志
        for (source, text) in batch.drain(..) {
            tracing::debug!(?task_id, %source, %text, "log (no app, skipped emit)");
        }
    }
}

/// 触发 emit（app 为 None 时仅记录日志）
fn emit_progress(
    app: &Option<AppHandle>,
    task_id: &TaskId,
    progress: u8,
    message: Option<String>,
    status: &str,
) {
    if let Some(app) = app {
        let evt = ProgressEvent {
            task_id: task_id.clone(),
            progress,
            message,
            status: status.to_string(),
        };
        if let Err(e) = app.emit("task_progress", &evt) {
            tracing::warn!(?task_id, error = %e, "failed to emit task_progress");
        }
    } else {
        tracing::debug!(?task_id, progress, status, "progress (no app, skipped emit)");
    }
}

/// 推送终态事件（completed/failed/cancelled）
///
/// `app = None` 时仅记录日志
///
/// **v3.5 扩展**：`payload` 字段携带 `TaskResult.payload`（任意 JSON），用于 async 命令
/// 把业务结果（SwitchPrerequisites / VersionCompatReport / SwitchVersionResult）传给前端。
/// 前端在 `task_completed` 事件中读 `e.payload.payload`。
pub(crate) fn emit_terminal(
    app: &Option<AppHandle>,
    task_id: &TaskId,
    status: &str,
    summary: Option<&str>,
    payload: Option<&serde_json::Value>,
) {
    if let Some(app) = app {
        #[derive(Serialize)]
        struct TerminalEvent<'a> {
            task_id: &'a str,
            status: &'a str,
            summary: Option<&'a str>,
            /// v3.5：TaskResult.payload（任意 JSON）
            payload: Option<&'a serde_json::Value>,
        }
        let evt = TerminalEvent {
            task_id,
            status,
            summary,
            payload,
        };
        if let Err(e) = app.emit("task_completed", &evt) {
            tracing::warn!(?task_id, error = %e, "failed to emit task_completed");
        }
    } else {
        tracing::info!(?task_id, status, "task terminal (no app, skipped emit)");
    }
}

/// 推送入队事件（submit 后立即触发）
///
/// `app = None` 时仅记录日志
pub(crate) fn emit_queued(
    app: &Option<AppHandle>,
    task_id: &TaskId,
    kind: &str,
    name: &str,
    priority: &str,
) {
    if let Some(app) = app {
        #[derive(Serialize)]
        struct QueuedEvent<'a> {
            task_id: &'a str,
            kind: &'a str,
            name: &'a str,
            priority: &'a str,
        }
        let evt = QueuedEvent {
            task_id,
            kind,
            name,
            priority,
        };
        if let Err(e) = app.emit("task_queued", &evt) {
            tracing::warn!(?task_id, error = %e, "failed to emit task_queued");
        }
    } else {
        tracing::info!(?task_id, kind, name, priority, "task queued (no app, skipped emit)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_progress_sender_clamps_percent() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sender = ProgressSender { tx };
        sender.send_percent(150);
        sender.send_percent(200);
        if let Some(ProgressMsg::Update(p)) = rx.recv().await {
            assert_eq!(p, 100);
        }
    }

    #[tokio::test]
    async fn test_progress_sender_message() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sender = ProgressSender { tx };
        sender.send_message("hello");
        if let Some(ProgressMsg::Message(m)) = rx.recv().await {
            assert_eq!(m, "hello");
        }
    }

    #[tokio::test]
    async fn test_spawn_flush_loop_no_app_no_panic() {
        // 无 AppHandle 也不应 panic，仅记录日志
        let (tx, rx) = mpsc::unbounded_channel();
        let sender = ProgressSender { tx };
        sender.send_percent(50);
        drop(sender);
        let handle = spawn_flush_loop(None, "t1".to_string(), rx);
        // 等待 flush loop 退出
        let _ = handle.await;
    }
}
