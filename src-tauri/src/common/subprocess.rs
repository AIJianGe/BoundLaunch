//! 统一子进程执行工具（v3.6 异步化改造）
//!
//! ## 设计动机
//!
//! v3.6 之前所有子进程调用使用 `tokio::time::timeout` 包裹，存在以下问题：
//! - 90s/300s 超时命中后报错，用户无法主动取消
//! - 超时是"硬错误"，不是"用户取消"语义
//! - 无法级联取消（父任务取消时子进程不知道）
//!
//! v3.6 改造：统一用 `CancellationToken` 替代 `tokio::time::timeout`，
//! 取消时显式 `start_kill()` 子进程，返回 `SubprocessError::Cancelled`。
//!
//! ## 使用方式
//!
//! ```ignore
//! use crate::common::subprocess::run_with_cancel;
//!
//! let cmd = crate::common::process_util::new_command("python")
//!     .args(["-c", script])
//!     .stdout(Stdio::piped())
//!     .stderr(Stdio::piped())
//!     .kill_on_drop(true);
//!
//! let output = run_with_cancel(cmd, &cancel).await?;
//! ```

use std::process::Stdio;

use tokio::process::Command;
use tokio_util::sync::CancellationToken;

/// 子进程错误类型
#[derive(Debug, thiserror::Error)]
pub enum SubprocessError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("操作已取消")]
    Cancelled,
    #[error("子进程退出码: {code}")]
    Exit { code: i32, stderr: String },
}

/// 带 CancellationToken 的异步子进程执行
///
/// - `cancel` 触发时，`child.start_kill()` + 返回 `Cancelled`
/// - 正常完成时返回 `Output`
/// - 调用方应确保 `cmd` 已配置 `kill_on_drop(true)`（双重保险）
///
/// **不捕获 stdout/stderr**：调用方需自行配置 `.stdout(Stdio::piped())` 等。
/// 若需要实时日志收集，请用 [`run_with_cancel_and_log`]。
///
/// v3.6 修复：用 `child.wait()` + 手动读 stdout/stderr 替代 `wait_with_output()`，
/// 避免 `wait_with_output` 消费 `child` 导致 cancel 分支无法调 `start_kill`。
///
/// v3.6.1：参数改为 `&mut Command`，因为 tokio `Command::args()` 等 builder 方法
/// 返回 `&mut Command`，调用方构建的命令链结果即为 `&mut Command`。函数内部会再调
/// 一次 `kill_on_drop(true)` 做双重保险。
pub async fn run_with_cancel(
    cmd: &mut Command,
    cancel: &CancellationToken,
) -> Result<std::process::Output, SubprocessError> {
    cmd.kill_on_drop(true);
    let mut child = cmd.spawn()?;

    // 取出 stdout/stderr 管道，避免 wait() 后管道被 drop
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let result = tokio::select! {
        status = child.wait() => {
            // 正常完成：读 stdout/stderr
            let stdout_buf = if let Some(mut s) = stdout {
                use tokio::io::AsyncReadExt;
                let mut buf = Vec::new();
                s.read_to_end(&mut buf).await.unwrap_or_default();
                buf
            } else {
                Vec::new()
            };
            let stderr_buf = if let Some(mut s) = stderr {
                use tokio::io::AsyncReadExt;
                let mut buf = Vec::new();
                s.read_to_end(&mut buf).await.unwrap_or_default();
                buf
            } else {
                Vec::new()
            };
            Ok(std::process::Output {
                status: status?,
                stdout: stdout_buf,
                stderr: stderr_buf,
            })
        }
        _ = cancel.cancelled() => {
            // 取消：显式 kill（kill_on_drop 也会做，但显式更可靠）
            let _ = child.start_kill();
            tracing::debug!("subprocess cancelled by CancellationToken");
            Err(SubprocessError::Cancelled)
        }
    };

    result
}

/// 带 CancellationToken + 实时日志收集的异步子进程执行
///
/// 用于 git / uv 等需要实时日志的长命令。
/// stdout 和 stderr 的每一行都会被推送到 `log_collector`。
///
/// **注意**：此函数会消费 stdout/stderr，返回的 `Output` 中 stdout/stderr 为空。
/// 如需获取完整输出，请用 [`run_with_cancel`]。
pub async fn run_with_cancel_and_log(
    cmd: &mut Command,
    cancel: &CancellationToken,
    log_collector: &std::sync::Arc<crate::common::line_collector::LineCollector>,
    source: &str,
) -> Result<std::process::Output, SubprocessError> {
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn()?;

    // 取 stdout / stderr 的管道
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // spawn 两个 task 逐行读 stdout / stderr → log_collector
    let collector_stdout = log_collector.clone();
    let source_stdout = source.to_string();
    let stdout_task = if let Some(stdout) = stdout {
        Some(tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                collector_stdout.push_with_source(source_stdout.clone(), line);
            }
        }))
    } else {
        None
    };

    let collector_stderr = log_collector.clone();
    let source_stderr = source.to_string();
    let stderr_task = if let Some(stderr) = stderr {
        Some(tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                collector_stderr.push_with_source(source_stderr.clone(), line);
            }
        }))
    } else {
        None
    };

    // 等待子进程完成或取消
    let result = tokio::select! {
        result = child.wait() => {
            let status = result?;
            // 等待 stdout/stderr task 完成（确保日志都收集到了）
            if let Some(t) = stdout_task { let _ = t.await; }
            if let Some(t) = stderr_task { let _ = t.await; }
            // wait_with_output 的 Output 需要 stdout/stderr 字段，
            // 但管道已被 take() 走，这里返回空 vec
            Ok(std::process::Output {
                status,
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
        _ = cancel.cancelled() => {
            let _ = child.start_kill();
            tracing::debug!("subprocess with log cancelled by CancellationToken");
            Err(SubprocessError::Cancelled)
        }
    };

    result
}
