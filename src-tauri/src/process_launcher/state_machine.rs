//! 进程状态机转换规则
//!
//! 详见 `PR/03-模块设计/06-ProcessLauncher.md §4.1 进程状态机`
//!
//! 状态图：
//! ```text
//! [Stopped]
//!    ↓ start()
//! [Starting] (spawn 子进程 + 健康检查轮询)
//!    ↓ 健康检查通过
//! [Running] (PID 已知)
//!    ↓ stop()
//! [Stopping] (POST /interrupt → SIGTERM → 等待 → SIGKILL)
//!    ↓ 进程退出
//! [Stopped]
//!
//! 异常分支：
//! [Starting] -- 健康检查超时 --> [Stopping] → [Stopped]
//! [Running]  -- 进程崩溃    --> [Crashed]
//! [Stopping] -- SIGKILL 后仍不退出 --> [Crashed]
//! ```

use chrono::Utc;

use crate::error::ProcessError;
use crate::process_launcher::models::{ProcessStatus, StopReason};

/// 状态转换规则结果：成功转换为 next，或失败返回 ProcessError
pub type TransitionResult = Result<ProcessStatus, ProcessError>;

/// Stopped → Starting
///
/// 仅允许在 Stopped / Crashed 状态触发。
/// Running / Starting / Stopping 状态调用返回 `AlreadyRunning` 或 `NotRunning` 错误。
pub fn transition_to_starting(current: &ProcessStatus, port: u16) -> TransitionResult {
    match current {
        ProcessStatus::Stopped | ProcessStatus::Crashed { .. } => Ok(ProcessStatus::Starting {
            started_at: Utc::now(),
            port,
        }),
        ProcessStatus::Running { pid, .. } => Err(ProcessError::AlreadyRunning { pid: *pid }),
        ProcessStatus::Starting { .. } => Err(ProcessError::AlreadyRunning { pid: 0 }),
        ProcessStatus::Stopping { .. } => Err(ProcessError::AlreadyRunning { pid: 0 }),
    }
}

/// Starting → Running
///
/// 仅允许在 Starting 状态触发（健康检查通过后）。
/// 保留原 started_at，但 PID 与 port 来自子进程。
pub fn transition_to_running(
    current: &ProcessStatus,
    pid: u32,
    port: u16,
) -> TransitionResult {
    match current {
        ProcessStatus::Starting { started_at, .. } => Ok(ProcessStatus::Running {
            pid,
            started_at: *started_at,
            port,
        }),
        ProcessStatus::Running { pid: existing_pid, .. } => {
            Err(ProcessError::AlreadyRunning { pid: *existing_pid })
        }
        ProcessStatus::Stopped => Err(ProcessError::ProcessExited),
        ProcessStatus::Stopping { .. } => Err(ProcessError::ProcessExited),
        ProcessStatus::Crashed { .. } => Err(ProcessError::ProcessExited),
    }
}

/// Starting / Running → Stopping
///
/// 仅允许在 Starting / Running 状态触发。
/// 已 Stopped / Crashed 状态调用返回 `NotRunning`（幂等错误）。
pub fn transition_to_stopping(
    current: &ProcessStatus,
    reason: StopReason,
) -> TransitionResult {
    match current {
        ProcessStatus::Starting { .. } | ProcessStatus::Running { .. } => {
            Ok(ProcessStatus::Stopping { reason })
        }
        ProcessStatus::Stopped | ProcessStatus::Crashed { .. } => Err(ProcessError::NotRunning),
        ProcessStatus::Stopping { .. } => {
            // 已在停止中：返回当前状态（幂等）
            Ok(current.clone())
        }
    }
}

/// Stopping → Stopped
///
/// 仅允许在 Stopping 状态触发（子进程正常退出）。
pub fn transition_to_stopped(current: &ProcessStatus) -> TransitionResult {
    match current {
        ProcessStatus::Stopping { .. } => Ok(ProcessStatus::Stopped),
        // 子进程可能在 Starting / Running 状态直接退出（崩溃前）
        ProcessStatus::Starting { .. } | ProcessStatus::Running { .. } => Ok(ProcessStatus::Stopped),
        // 已停止：幂等返回
        ProcessStatus::Stopped => Ok(ProcessStatus::Stopped),
        // 已崩溃：保持 Crashed 状态
        ProcessStatus::Crashed { .. } => Ok(current.clone()),
    }
}

/// any → Crashed
///
/// 子进程异常退出时触发。
/// `exit_code` 为 None 表示被信号杀死，Some(code) 表示进程返回非零退出码。
pub fn transition_to_crashed(
    _current: &ProcessStatus,
    exit_code: Option<i32>,
    error: String,
) -> ProcessStatus {
    ProcessStatus::Crashed {
        exit_code,
        error,
        at: Utc::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stopped_to_starting() {
        let next = transition_to_starting(&ProcessStatus::Stopped, 8188).unwrap();
        assert!(matches!(next, ProcessStatus::Starting { port: 8188, .. }));
    }

    #[test]
    fn test_crashed_to_starting() {
        let crashed = ProcessStatus::Crashed {
            exit_code: Some(1),
            error: "boom".into(),
            at: Utc::now(),
        };
        let next = transition_to_starting(&crashed, 8188).unwrap();
        assert!(matches!(next, ProcessStatus::Starting { .. }));
    }

    #[test]
    fn test_running_to_starting_returns_already_running() {
        let running = ProcessStatus::Running {
            pid: 1234,
            started_at: Utc::now(),
            port: 8188,
        };
        let err = transition_to_starting(&running, 8188).unwrap_err();
        assert!(matches!(err, ProcessError::AlreadyRunning { pid: 1234 }));
    }

    #[test]
    fn test_starting_to_running() {
        let starting = ProcessStatus::Starting {
            started_at: Utc::now(),
            port: 8188,
        };
        let next = transition_to_running(&starting, 999, 8188).unwrap();
        match next {
            ProcessStatus::Running { pid, .. } => assert_eq!(pid, 999),
            _ => panic!("expected Running"),
        }
    }

    #[test]
    fn test_running_to_running_returns_already_running() {
        let running = ProcessStatus::Running {
            pid: 1234,
            started_at: Utc::now(),
            port: 8188,
        };
        let err = transition_to_running(&running, 999, 8188).unwrap_err();
        assert!(matches!(err, ProcessError::AlreadyRunning { pid: 1234 }));
    }

    #[test]
    fn test_running_to_stopping() {
        let running = ProcessStatus::Running {
            pid: 1234,
            started_at: Utc::now(),
            port: 8188,
        };
        let next = transition_to_stopping(&running, StopReason::UserRequested).unwrap();
        assert!(matches!(
            next,
            ProcessStatus::Stopping {
                reason: StopReason::UserRequested
            }
        ));
    }

    #[test]
    fn test_stopped_to_stopping_returns_not_running() {
        let err = transition_to_stopping(&ProcessStatus::Stopped, StopReason::UserRequested)
            .unwrap_err();
        assert!(matches!(err, ProcessError::NotRunning));
    }

    #[test]
    fn test_stopping_to_stopping_idempotent() {
        let stopping = ProcessStatus::Stopping {
            reason: StopReason::UserRequested,
        };
        let next = transition_to_stopping(&stopping, StopReason::HealthCheckTimeout).unwrap();
        // 保持原 reason（幂等：不覆盖正在进行的停止原因）
        assert!(matches!(
            next,
            ProcessStatus::Stopping {
                reason: StopReason::UserRequested
            }
        ));
    }

    #[test]
    fn test_stopping_to_stopped() {
        let stopping = ProcessStatus::Stopping {
            reason: StopReason::UserRequested,
        };
        let next = transition_to_stopped(&stopping).unwrap();
        assert_eq!(next, ProcessStatus::Stopped);
    }

    #[test]
    fn test_stopped_to_stopped_idempotent() {
        let next = transition_to_stopped(&ProcessStatus::Stopped).unwrap();
        assert_eq!(next, ProcessStatus::Stopped);
    }

    #[test]
    fn test_running_to_crashed() {
        let running = ProcessStatus::Running {
            pid: 1234,
            started_at: Utc::now(),
            port: 8188,
        };
        let next = transition_to_crashed(&running, Some(1), "exit code 1".into());
        assert!(matches!(next, ProcessStatus::Crashed { exit_code: Some(1), .. }));
    }

    #[test]
    fn test_status_transitions_full_lifecycle() {
        // 完整生命周期：Stopped → Starting → Running → Stopping → Stopped
        let s0 = ProcessStatus::Stopped;
        let s1 = transition_to_starting(&s0, 8188).unwrap();
        assert!(s1.is_alive());

        let s2 = transition_to_running(&s1, 1234, 8188).unwrap();
        assert!(s2.is_running());

        let s3 = transition_to_stopping(&s2, StopReason::UserRequested).unwrap();
        assert!(matches!(s3, ProcessStatus::Stopping { .. }));

        let s4 = transition_to_stopped(&s3).unwrap();
        assert!(s4.is_terminal());
    }
}
