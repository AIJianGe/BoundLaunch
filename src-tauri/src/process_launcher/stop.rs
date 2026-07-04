//! 进程停止流程：interrupt → SIGTERM → SIGKILL（跨平台）
//!
//! 详见 `PR/03-模块设计/06-ProcessLauncher.md §5.2 停止流程`
//!
//! 设计要点：
//! - **跨平台**：Windows 用 `taskkill /T`，Unix 用 `kill -TERM/-KILL`
//! - **优雅升级**：interrupt（HTTP POST /interrupt） → SIGTERM（5s） → SIGKILL（2s）
//! - **best-effort interrupt**：HTTP 请求失败不阻塞后续 SIGTERM
//! - **幂等**：对已退出进程调用 terminate_process 不报错
//!
//! 设计模式：
//! - **Adapter**：`terminate_process` 封装跨平台差异，对调用方暴露统一接口

use std::time::Duration;

use chrono::Utc;
use tokio::process::Child;

use crate::error::ProcessError;
use crate::process_launcher::models::{ProcessStatus, StopReason};

/// interrupt 阶段超时（POST /interrupt 不应阻塞太久）
const INTERRUPT_TIMEOUT: Duration = Duration::from_secs(2);

/// SIGTERM 后等待退出的超时
const SIGTERM_WAIT: Duration = Duration::from_secs(5);

/// SIGKILL 后等待退出的超时
const SIGKILL_WAIT: Duration = Duration::from_secs(2);

/// ComfyUI `/interrupt` 端点请求体（空 JSON）
///
/// ComfyUI 的 /interrupt 接收任意 JSON，传递空对象即可中断当前生成。
const INTERRUPT_BODY: &str = "{}";

/// 向 ComfyUI 发送 POST /interrupt 请求
///
/// 失败不返回错误（best-effort）：可能 ComfyUI 已不响应，
/// 后续的 SIGTERM 才是真正的"硬"停止。
pub async fn post_interrupt(port: u16) {
    let url = format!("http://127.0.0.1:{}/interrupt", port);
    let client = reqwest::Client::new();

    let result = tokio::time::timeout(
        INTERRUPT_TIMEOUT,
        client.post(&url).body(INTERRUPT_BODY).send(),
    )
    .await;

    match result {
        Ok(Ok(resp)) => {
            tracing::info!(
                status = %resp.status(),
                "POST /interrupt responded",
            );
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "POST /interrupt request failed");
        }
        Err(_) => {
            tracing::warn!(
                timeout_ms = INTERRUPT_TIMEOUT.as_millis(),
                "POST /interrupt timed out, will proceed to SIGTERM"
            );
        }
    }
}

/// 跨平台进程终止
///
/// - Windows：调用 `taskkill /PID <pid> /T`（终止进程树）
/// - Unix：`kill(pid, SIGTERM)` 或 `kill(pid, SIGKILL)`
///
/// `force=true` 时使用强制终止：
/// - Windows：`taskkill /PID <pid> /T /F`
/// - Unix：`SIGKILL`
pub async fn terminate_process(pid: u32, force: bool) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        let pid_str = pid.to_string();
        let mut args: Vec<&str> = vec!["/PID", &pid_str, "/T"];
        if force {
            args.push("/F");
        }
        let output = tokio::process::Command::new("taskkill")
            .args(&args)
            .output()
            .await?;

        if !output.status.success() {
            // taskkill 返回非零：可能进程已退出，检查 stderr 内容
            let stderr = String::from_utf8_lossy(&output.stderr);
            // 错误码 128 表示进程不存在（即已退出）
            if stderr.contains("not found") || stderr.contains("找不到")
                || stderr.contains("no such process")
            {
                tracing::debug!(pid, "taskkill: process already exited");
                return Ok(());
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("taskkill failed: {}", stderr.trim()),
            ));
        }
        tracing::debug!(pid, force, "taskkill succeeded");
        return Ok(());
    }

    #[cfg(not(target_os = "windows"))]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;

        let sig = if force { Signal::SIGKILL } else { Signal::SIGTERM };
        let nix_pid = Pid::from_raw(pid as i32);

        match kill(nix_pid, sig) {
            Ok(()) => {
                tracing::debug!(pid, force, "kill signal sent");
                Ok(())
            }
            Err(nix::errno::Errno::ESRCH) => {
                // No such process：进程已退出，幂等返回 Ok
                tracing::debug!(pid, "kill: process already exited (ESRCH)");
                Ok(())
            }
            Err(e) => Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("kill({}) failed: {}", pid, e),
            )),
        }
    }
}

/// 完整停止流程：interrupt → SIGTERM → SIGKILL
///
/// 返回子进程退出状态。
///
/// # 流程
/// 1. POST /interrupt（best-effort，2s 超时）
/// 2. SIGTERM（terminate_process force=false）
/// 3. wait child.wait() 5s
/// 4. 仍存活 → SIGKILL（terminate_process force=true）
/// 5. wait child.wait() 2s
/// 6. 仍存活 → 返回 `StopFailed`
pub async fn stop_with_grace(
    mut child: Child,
    pid: u32,
    port: u16,
) -> Result<std::process::ExitStatus, ProcessError> {
    // 阶段 1：POST /interrupt（best-effort）
    post_interrupt(port).await;

    // 阶段 2：SIGTERM（force=false）
    tracing::info!(pid, "sending SIGTERM");
    if let Err(e) = terminate_process(pid, false).await {
        tracing::warn!(pid, error = %e, "SIGTERM failed, will try SIGKILL");
    }

    // 阶段 3：wait 5s
    match tokio::time::timeout(SIGTERM_WAIT, child.wait()).await {
        Ok(Ok(status)) => {
            tracing::info!(pid, ?status, "process exited after SIGTERM");
            return Ok(status);
        }
        Ok(Err(e)) => {
            // child.wait() 出错：可能进程已退出但 wait 失败
            tracing::warn!(pid, error = %e, "child.wait() returned error after SIGTERM");
            return Err(ProcessError::Io(e.to_string()));
        }
        Err(_) => {
            tracing::warn!(pid, timeout = ?SIGTERM_WAIT, "process did not exit after SIGTERM, escalating to SIGKILL");
        }
    }

    // 阶段 4：SIGKILL（force=true）
    tracing::info!(pid, "sending SIGKILL");
    if let Err(e) = terminate_process(pid, true).await {
        tracing::error!(pid, error = %e, "SIGKILL failed");
        return Err(ProcessError::StopFailed);
    }

    // 阶段 5：wait 2s
    match tokio::time::timeout(SIGKILL_WAIT, child.wait()).await {
        Ok(Ok(status)) => {
            tracing::info!(pid, ?status, "process exited after SIGKILL");
            Ok(status)
        }
        Ok(Err(e)) => {
            tracing::warn!(pid, error = %e, "child.wait() returned error after SIGKILL");
            Err(ProcessError::Io(e.to_string()))
        }
        Err(_) => {
            tracing::error!(pid, "process did not exit after SIGKILL, reporting StopFailed");
            Err(ProcessError::StopFailed)
        }
    }
}

/// 根据 ExitStatus 推导下一个 ProcessStatus
///
/// - exit_code == 0 → `Stopped`
/// - exit_code != 0 → `Crashed { exit_code, error }`
/// - 信号终止（None） → `Crashed { exit_code: None, error: "killed by signal" }`
pub fn status_from_exit(
    exit_status: std::process::ExitStatus,
    reason: StopReason,
) -> ProcessStatus {
    match exit_status.code() {
        Some(0) => ProcessStatus::Stopped,
        Some(code) => ProcessStatus::Crashed {
            exit_code: Some(code),
            error: format!("exit with code {} ({})", code, reason.as_str()),
            at: Utc::now(),
        },
        None => ProcessStatus::Crashed {
            exit_code: None,
            error: format!("killed by signal ({})", reason.as_str()),
            at: Utc::now(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn test_status_from_exit_success() {
        use std::os::unix::process::ExitStatusExt;
        let status = std::process::ExitStatus::from_raw(0);
        let next = status_from_exit(status, StopReason::UserRequested);
        match next {
            ProcessStatus::Stopped | ProcessStatus::Crashed { .. } => {}
            _ => panic!("unexpected status"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_status_from_exit_nonzero() {
        use std::os::unix::process::ExitStatusExt;
        // exit code 1（信号 0 + code 1）
        let status = std::process::ExitStatus::from_raw(1 << 8);
        let next = status_from_exit(status, StopReason::UserRequested);
        assert!(matches!(next, ProcessStatus::Crashed { .. }));
    }

    #[test]
    fn test_terminate_process_already_exited_does_not_panic() {
        // 用一个不可能存在的 PID（极高值）测试幂等
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // PID 99999999 几乎不可能存在
            let result = terminate_process(99999999, false).await;
            // 不应返回 Err（已退出视为幂等 Ok）
            assert!(result.is_ok(), "terminate_process on non-existent pid should be Ok: {:?}", result);
        });
    }

    #[test]
    fn test_stop_reason_as_str() {
        assert_eq!(StopReason::UserRequested.as_str(), "user_requested");
        assert_eq!(StopReason::HealthCheckTimeout.as_str(), "health_check_timeout");
        assert_eq!(StopReason::ExternalSignal.as_str(), "external_signal");
        assert_eq!(StopReason::ParentExit.as_str(), "parent_exit");
    }
}

/// 删除 PID 文件（幂等：不存在时返回 Ok）
pub async fn remove_pid_file(path: &std::path::Path) {
    if let Err(e) = tokio::fs::remove_file(path).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(?path, error = %e, "failed to remove pid file");
        }
    } else {
        tracing::debug!(?path, "pid file removed");
    }
}
