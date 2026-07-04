//! 日志管道：mpsc + RingBuffer + LogStore 三层
//!
//! 详见 `PR/03-模块设计/06-ProcessLauncher.md §4.4 日志环形缓冲`
//!
//! 设计要点：
//! - **写端无锁**：stdout/stderr 行通过 `mpsc::UnboundedSender` 推送
//! - **后台聚合**：100ms flush 一次累积行，避免高频锁竞争
//! - **三层落地**：前端 emit → RingBuffer（内存） → LogStore（SQLite 持久化）
//! - **降级策略**：LogStore 写入失败不影响 RingBuffer 与前端推送，仅 warn
//!
//! 设计模式：
//! - **Decorator**：在原始 stdout/stderr 行流上叠加聚合 / 持久化能力
//! - **Producer-Consumer**：mpsc 解耦生产者（reader task）与消费者（flush task）

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use parking_lot::RwLock;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::log_store::LogStoreService;
use crate::log_store::repository::{LogEntry, LogLevel};

use super::ring_buffer::RingBuffer;

/// 默认环形缓冲容量（行）
pub const DEFAULT_BUFFER_CAPACITY: usize = 5000;

/// 后台 flush 间隔
const FLUSH_INTERVAL_MS: u64 = 100;

/// 单次 batch 上限（避免单帧推送过多日志）
const BATCH_SIZE_LIMIT: usize = 100;

/// 单条日志推送至前端的事件载荷
#[derive(Debug, Clone, Serialize)]
struct LogLineEvent<'a> {
    /// 来源（"stdout" / "stderr"）
    source: &'a str,
    /// 行内容
    line: &'a str,
    /// 时间戳（ISO 8601）
    ts: chrono::DateTime<Utc>,
}

/// 日志管道
///
/// - 持有 mpsc sender（多个 reader task 共享 clone）
/// - 持有 RingBuffer 的 Arc（后台 task 与 tail_log 共享）
/// - 持有后台 flush task 的 JoinHandle（drop 时 detach）
pub struct LogPipeline {
    tx: mpsc::UnboundedSender<PendingLine>,
    buffer: Arc<RwLock<RingBuffer<String>>>,
    /// 后台 flush task 句柄（仅供 shutdown 等待）
    _join: JoinHandle<()>,
}

/// 待处理日志行
struct PendingLine {
    source: String,
    line: String,
    ts: chrono::DateTime<Utc>,
}

impl LogPipeline {
    /// 创建日志管道并启动后台 flush task
    ///
    /// - `capacity`：RingBuffer 容量（建议 5000）
    /// - `log_store`：LogStore 服务（None 时跳过持久化）
    /// - `app`：Tauri 句柄（None 时跳过前端 emit，测试场景）
    pub fn new(
        capacity: usize,
        log_store: Arc<LogStoreService>,
        app: Option<AppHandle>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let buffer = Arc::new(RwLock::new(RingBuffer::<String>::new(capacity)));

        let join = spawn_flush_loop(rx, buffer.clone(), log_store, app);

        Self {
            tx,
            buffer,
            _join: join,
        }
    }

    /// 推送一行日志（无锁，仅 mpsc send）
    ///
    /// 失败说明后台 task 已退出（例如进程停止后 LogPipeline 被 drop），忽略
    pub fn push(&self, source: &str, line: String) {
        let _ = self.tx.send(PendingLine {
            source: source.to_string(),
            ts: Utc::now(),
            line,
        });
    }

    /// 读取最近 n 条日志（按时间倒序：最新的在前）
    pub fn tail(&self, n: usize) -> Vec<String> {
        self.buffer.read().tail_cloned(n)
    }

    /// 当前缓冲区长度
    pub fn len(&self) -> usize {
        self.buffer.read().len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.buffer.read().is_empty()
    }

    /// 清空缓冲区
    pub fn clear(&self) {
        self.buffer.write().clear();
    }
}

/// 启动后台 flush 循环：100ms 聚合一次累积行，写入 RingBuffer + LogStore + emit 前端
///
/// 退出条件：所有 sender drop 后 `rx.recv()` 返回 None
fn spawn_flush_loop(
    mut rx: mpsc::UnboundedReceiver<PendingLine>,
    buffer: Arc<RwLock<RingBuffer<String>>>,
    log_store: Arc<LogStoreService>,
    app: Option<AppHandle>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut batch: Vec<PendingLine> = Vec::with_capacity(BATCH_SIZE_LIMIT);
        let mut interval = tokio::time::interval(Duration::from_millis(FLUSH_INTERVAL_MS));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(line) => {
                        batch.push(line);
                        if batch.len() >= BATCH_SIZE_LIMIT {
                            flush(&mut batch, &buffer, &log_store, &app).await;
                        }
                    }
                    None => {
                        // 所有 sender drop，flush 残留并退出
                        if !batch.is_empty() {
                            flush(&mut batch, &buffer, &log_store, &app).await;
                        }
                        tracing::debug!("log_pipeline flush loop exiting");
                        break;
                    }
                },
                _ = interval.tick() => {
                    if !batch.is_empty() {
                        flush(&mut batch, &buffer, &log_store, &app).await;
                    }
                }
            }
        }
    })
}

/// 批量写入三层：前端 emit / RingBuffer / LogStore
async fn flush(
    batch: &mut Vec<PendingLine>,
    buffer: &Arc<RwLock<RingBuffer<String>>>,
    log_store: &Arc<LogStoreService>,
    app: &Option<AppHandle>,
) {
    if batch.is_empty() {
        return;
    }

    // 1. 推送至前端（按行 emit，前端按 ts 排序展示）
    if let Some(app) = app {
        for line in batch.iter() {
            let evt = LogLineEvent {
                source: &line.source,
                line: &line.line,
                ts: line.ts,
            };
            if let Err(e) = app.emit("comfyui_log", &evt) {
                tracing::warn!(error = %e, "emit comfyui_log failed");
            }
        }
    }

    // 2. 写入 RingBuffer（持写锁，瞬间操作）
    {
        let mut buf = buffer.write();
        for line in batch.iter() {
            buf.push(line.line.clone());
        }
    }

    // 3. 持久化到 LogStore（批量插入，失败仅 warn）
    let entries: Vec<LogEntry> = batch
        .iter()
        .map(|p| LogEntry {
            timestamp: p.ts,
            level: detect_log_level(&p.line),
            source: format!("comfyui:{}", p.source),
            message: p.line.clone(),
        })
        .collect();
    if let Err(e) = log_store.logs().append_batch(&entries).await {
        tracing::warn!(error = %e, "LogStore persist failed, falling back to memory-only buffer");
    }

    batch.clear();
}

/// 简单的日志级别识别（基于行内容启发式判断）
///
/// ComfyUI 的 stdout 不带结构化级别，这里按关键字识别 ERROR/WARN，其余默认 INFO。
fn detect_log_level(line: &str) -> LogLevel {
    let lower = line.to_lowercase();
    if lower.contains("error") || lower.contains("traceback") || lower.contains("exception") {
        LogLevel::Error
    } else if lower.contains("warn") {
        LogLevel::Warn
    } else if lower.contains("debug") {
        LogLevel::Debug
    } else {
        LogLevel::Info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_store::LogStoreService;

    async fn make_log_store() -> Arc<LogStoreService> {
        Arc::new(LogStoreService::new(None).await.expect("init logstore"))
    }

    #[tokio::test]
    async fn test_pipeline_push_and_tail() {
        let log_store = make_log_store().await;
        let pipeline = LogPipeline::new(100, log_store, None);

        pipeline.push("stdout", "line 1".into());
        pipeline.push("stdout", "line 2".into());
        pipeline.push("stderr", "error occurred".into());

        // 给后台 flush 一点时间
        tokio::time::sleep(Duration::from_millis(200)).await;

        let tail = pipeline.tail(10);
        assert_eq!(tail.len(), 3);
        // 倒序：最新在前
        assert_eq!(tail[0], "error occurred");
        assert_eq!(tail[2], "line 1");
    }

    #[tokio::test]
    async fn test_pipeline_ring_buffer_eviction() {
        let log_store = make_log_store().await;
        let pipeline = LogPipeline::new(3, log_store, None);

        // push 5 行（容量 3，应保留最后 3 行）
        for i in 0..5 {
            pipeline.push("stdout", format!("line {}", i));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;

        let tail = pipeline.tail(10);
        assert_eq!(tail.len(), 3);
        // 最新的在前：line 4 / line 3 / line 2
        assert_eq!(tail[0], "line 4");
        assert_eq!(tail[2], "line 2");
    }

    #[tokio::test]
    async fn test_pipeline_clear() {
        let log_store = make_log_store().await;
        let pipeline = LogPipeline::new(10, log_store, None);
        pipeline.push("stdout", "line 1".into());
        tokio::time::sleep(Duration::from_millis(200)).await;

        pipeline.clear();
        assert!(pipeline.is_empty());
    }

    #[test]
    fn test_detect_log_level() {
        assert_eq!(detect_log_level("ERROR: foo"), LogLevel::Error);
        assert_eq!(detect_log_level("Traceback (most recent call last)"), LogLevel::Error);
        assert_eq!(detect_log_level("WARNING: bar"), LogLevel::Warn);
        assert_eq!(detect_log_level("debug: stuff"), LogLevel::Debug);
        assert_eq!(detect_log_level("just info"), LogLevel::Info);
    }

    #[tokio::test]
    async fn test_pipeline_no_app_no_panic() {
        // 验证无 AppHandle 时不会 panic
        let log_store = make_log_store().await;
        let pipeline = LogPipeline::new(10, log_store, None);
        pipeline.push("stdout", "test".into());
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(pipeline.tail(10).len(), 1);
    }
}
