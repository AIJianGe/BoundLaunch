//! 跨平台进程终止 + OS 检测
//!
//! 设计模式：适配器 (Adapter)
//! 原始接口：Windows taskkill / Linux kill
//! 统一接口：terminate_process(pid, force)
//!
//! 详见 `PR/03-模块设计/06-ProcessLauncher.md §5.2`

use std::io;

/// 获取当前操作系统的标准化字符串（v3.0 新增，F25）
///
/// 返回值：`"windows"` | `"linux"` | `"macos"`
/// 用于 TorchVariant 平台兼容性检查（system::recommend）。
pub fn current_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// 终止进程
///
/// force=false: 优雅终止（SIGTERM / taskkill /T）
/// force=true:  强制终止（SIGKILL / taskkill /T /F）
pub async fn terminate_process(pid: u32, force: bool) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        let mut args = vec!["/PID".to_string(), pid.to_string(), "/T".to_string()];
        if force {
            args.push("/F".to_string());
        }
        // v3.3：使用 new_command 在 Windows 上加 CREATE_NO_WINDOW，避免弹 cmd 窗口
        let status = crate::common::process_util::new_command("taskkill")
            .args(&args)
            .status()
            .await?;

        if !status.success() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("taskkill exited with status {}", status),
            ));
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;

        let sig = if force { Signal::SIGKILL } else { Signal::SIGTERM };
        kill(Pid::from_raw(pid as i32), sig).map_err(|e| {
            io::Error::new(io::ErrorKind::Other, format!("kill failed: {}", e))
        })?;
    }

    Ok(())
}

/// 检测进程是否存在
pub async fn process_exists(pid: u32) -> bool {
    #[cfg(target_os = "windows")]
    {
        // v3.3：使用 new_command 在 Windows 上加 CREATE_NO_WINDOW，避免弹 cmd 窗口
        let output = crate::common::process_util::new_command("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .output()
            .await;
        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                !stdout.contains("INFO: No tasks") && stdout.contains(&pid.to_string())
            }
            Err(_) => false,
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        // kill(pid, 0) 不实际发送信号，仅检查存在性
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;
        kill(Pid::from_raw(pid as i32), None).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_terminate_invalid_pid_errors() {
        // 用一个不可能存在的 PID 测试
        let result = terminate_process(u32::MAX, true).await;
        assert!(result.is_err(), "should error for invalid pid");
    }

    #[tokio::test]
    async fn test_process_exists_for_invalid_pid() {
        assert!(!process_exists(u32::MAX).await);
    }
}
