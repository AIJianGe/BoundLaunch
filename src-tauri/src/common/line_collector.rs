//! 实时日志收集器（StderrTail / StdoutTail）
//!
//! v3.5：用于 CoreManager / PythonEnvManager 的子进程输出收集，
//! 同时支持：
//! 1. **实时推送**：通过 `mpsc::UnboundedSender<String>` 把每一行推送给前端
//! 2. **历史快照**：通过 `snapshot(n)` 拿最近 N 行（用于错误信息上下文）
//!
//! ## 设计动机
//!
//! 用户反馈："不仅要有百分比进度，还希望下面有执行的实时日志。
//!          好看哪里出问题发给你。" —— v3.5 需求
//!
//! 之前 v3.4 的 `LogPipeline` 是「聚合 100ms flush 一次」的模式，
//! 适合 ComfyUI 启动后的大流量日志，但**不适合子任务进度报告**：
//! - 用户期望看到 uv pip install 的「Resolved 5 packages / Downloaded torch-2.4.0」
//!   这种逐行反馈，而不是聚合 100 行再一次性 emit
//! - 失败时需要最近 N 行的 stderr 上下文（已成功发送的实时日志+历史的尾部）
//!
//! ## 架构
//!
//! ```text
//! 子进程 stdout/stderr (tokio::process::Command::stdout())
//!   ↓ BufReader::lines() 异步逐行
//! LineCollector (tokio::spawn 后台 task)
//!   ├→ push 到 RingBuffer (用于 snapshot 错误上下文)
//!   └→ 发送到 mpsc::UnboundedSender
//!         ↓
//! 父任务 forwarder (tokio::spawn 后台 task)
//!   └→ 从 mpsc 接收，转发到 ProgressSender.send_log()
//!         ↓
//! ProgressSender → 100ms 聚合 → emit "task_progress" event
//!         ↓
//! 前端 useTaskProgress.onLog() → 追加到 UI 日志面板
//! ```
//!
//! ## 与 LogPipeline 的关系
//!
//! - **LogPipeline**：ComfyUI 启动后的 stdout/stderr（量大、需要聚合 + SQLite 持久化）
//! - **LineCollector**：长任务（uv / git）期间的 stdout/stderr（量小、需要实时 + 错误快照）
//!
//! 两者并存，互不冲突。LineCollector 在 task action 内部用，task 结束后自动析构。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::common::ansi::strip_ansi;

/// 实时日志行类型
#[derive(Debug, Clone)]
pub struct LogLine {
    /// 来源标识（如 "git fetch" / "uv pip install"），用于前端分组显示
    pub source: String,
    /// 时间戳（ms since epoch）
    pub ts_ms: u64,
    /// 已清洗 ANSI 的纯文本
    pub text: String,
}

/// 线程安全的实时日志收集器
///
/// 同时提供：
/// - `push(text)` / `push_with_source(source, text)`：写入（无锁，由 LineCollector 后台 task 调用）
/// - `subscribe()`：拿一个 mpsc::Receiver，前端 UI / 父任务 forwarder 用
/// - `snapshot(n)`：拿最近 N 行（用于错误信息）
/// - `lines()`：拿所有历史（用于 LogStore 持久化，可选）
pub struct LineCollector {
    /// 内部环形缓冲（保存最近 N 行），用于 `snapshot`
    buffer: Mutex<VecDeque<LogLine>>,
    /// 环形缓冲容量
    capacity: usize,
    /// 实时推送的多消费者 sender
    tx: mpsc::UnboundedSender<LogLine>,
}

impl LineCollector {
    /// 创建新的 LineCollector
    ///
    /// - `capacity`：环形缓冲容量（默认 1000）
    /// - 返回 `(Arc<LineCollector>, broadcast receiver-like mpsc::Receiver)`
    pub fn new(capacity: usize) -> (Arc<Self>, mpsc::UnboundedReceiver<LogLine>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let collector = Arc::new(Self {
            buffer: Mutex::new(VecDeque::with_capacity(capacity.min(4096))),
            capacity,
            tx,
        });
        (collector, rx)
    }

    /// 写入一行日志（带 source）
    pub fn push_with_source(&self, source: impl Into<String>, text: impl Into<String>) {
        let text = text.into();
        let clean = strip_ansi(&text);
        let line = LogLine {
            source: source.into(),
            ts_ms: chrono::Utc::now().timestamp_millis() as u64,
            text: clean,
        };

        // 1. 推送到环形缓冲（带锁，但只 pop_front/push_back，极快）
        {
            let mut buf = self.buffer.lock().expect("LineCollector buffer poisoned");
            if buf.len() == self.capacity {
                buf.pop_front();
            }
            buf.push_back(line.clone());
        }

        // 2. 实时推送给订阅者（无锁 send，失败也无妨——订阅者可能已 drop）
        let _ = self.tx.send(line);
    }

    /// 写入一行日志（默认 source = ""）
    pub fn push(&self, text: impl Into<String>) {
        self.push_with_source("", text);
    }

    /// 拿最近 N 行（用于错误信息上下文）
    pub fn snapshot(&self, n: usize) -> Vec<String> {
        let buf = self.buffer.lock().expect("LineCollector buffer poisoned");
        buf.iter()
            .rev()
            .take(n)
            .map(|l| {
                if l.source.is_empty() {
                    l.text.clone()
                } else {
                    format!("[{}] {}", l.source, l.text)
                }
            })
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    /// 拿最近 N 行（含 source 和 timestamp），结构化输出
    pub fn snapshot_structured(&self, n: usize) -> Vec<LogLine> {
        let buf = self.buffer.lock().expect("LineCollector buffer poisoned");
        buf.iter()
            .rev()
            .take(n)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    /// 当前缓冲中的行数
    pub fn len(&self) -> usize {
        self.buffer.lock().expect("LineCollector buffer poisoned").len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 清空缓冲（一般不需要，task 结束后 collector 自动 drop）
    pub fn clear(&self) {
        self.buffer
            .lock()
            .expect("LineCollector buffer poisoned")
            .clear();
    }
}

/// 便捷函数：spawn 一个后台 task，把子进程 stdout/stderr 异步逐行推送到 LineCollector
///
/// 用法：
/// ```ignore
/// use tokio::io::{AsyncBufReadExt, BufReader};
/// use tokio::process::Stdio;
///
/// let mut child = Command::new("uv")
///     .args(["pip", "install", "torch"])
///     .stdout(Stdio::piped())
///     .stderr(Stdio::piped())
///     .kill_on_drop(true)
///     .spawn()?;
///
/// spawn_collect_lines("uv", child.stdout.take().unwrap(), collector.clone());
/// spawn_collect_lines("uv", child.stderr.take().unwrap(), collector.clone());
///
/// let status = child.wait().await?;
/// ```
pub fn spawn_collect_lines<R>(source: &'static str, reader: R, collector: Arc<LineCollector>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    use tokio::io::{AsyncBufReadExt, BufReader};

    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            collector.push_with_source(source, line);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_push_and_snapshot() {
        let (c, _rx) = LineCollector::new(100);
        c.push("hello");
        c.push_with_source("git", "fetched");
        c.push_with_source("git", "[32mcolored[0m");

        let snap = c.snapshot(10);
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0], "hello");
        assert_eq!(snap[1], "[git] fetched");
        assert_eq!(snap[2], "[git] colored"); // ANSI 已被清洗
    }

    #[tokio::test]
    async fn test_ring_buffer_overflow() {
        let (c, _rx) = LineCollector::new(3);
        for i in 0..10 {
            c.push(format!("line{}", i));
        }
        let snap = c.snapshot(10);
        // 容量为 3，应该只保留最后 3 行
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0], "line7");
        assert_eq!(snap[1], "line8");
        assert_eq!(snap[2], "line9");
    }

    #[tokio::test]
    async fn test_subscribe() {
        let (c, mut rx) = LineCollector::new(100);
        c.push("line1");
        c.push("line2");

        // 实时订阅
        let l1 = rx.recv().await.unwrap();
        let l2 = rx.recv().await.unwrap();
        assert_eq!(l1.text, "line1");
        assert_eq!(l2.text, "line2");
    }

    #[tokio::test]
    async fn test_empty_collector() {
        let (c, _rx) = LineCollector::new(100);
        assert!(c.is_empty());
        assert_eq!(c.len(), 0);
        assert!(c.snapshot(10).is_empty());
    }
}
