//! 跨平台子进程创建工具
//!
//! 详见 `PR/03-模块设计/06-ProcessLauncher.md §11 消除 cmd 窗口`
//!
//! ## 背景
//!
//! Tauri release 模式用 `#![windows_subsystem = "windows"]`，主进程没有控制台。
//! Windows 上 `tokio::process::Command::new(...).spawn()` 不带 `creation_flags` 时，
//! 子进程会**自己开一个 cmd 窗口**运行（即使 stdin/stdout 已被重定向），
//! 表现为用户看到"莫名其妙弹个 cmd 窗口然后消失"。
//!
//! 解决方法：在 Windows 上给子进程加 `CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP` flag。
//! 跨平台：非 Windows 上等价于 `Command::new`。
//!
//! ## 使用方式
//!
//! 替换 `tokio::process::Command::new(program)` 为 `crate::common::process_util::new_command(program)`。
//! 其余 API 完全一致（返回 `tokio::process::Command`）。
//!
//! ## 影响范围
//!
//! 当前已改造 5 个调用点：
//! - `python_env/uv_runner.rs::run_cmd`（uv 子进程）
//! - `env_inspector/scripts.rs::run_python_script` & `run_pip_list` & `run_pip_list_fallback`
//! - `env_inspector/gpu.rs::try_detect_nvidia`
//! - `python_env/verify.rs::probe_python_version`
//! - `python_env/recovery.rs::check_packages` & `uv_run_cmd`
//!
//! ## 设计模式
//!
//! - **Adapter**：把"Windows 创建 flag"的复杂性隐藏在统一入口
//! - **Facade**：跨平台语义封装
//!
//! ## 不改造的调用点
//!
//! - `process_launcher/start.rs::spawn_comfyui_process`：已有自己的 `creation_flags(CREATE_NO_WINDOW.0 | CREATE_NEW_PROCESS_GROUP.0)` 设置（早于本模块）
//! - `env_inspector/gpu.rs::detect_cpu_model` (macOS 路径用 `std::process::Command`)：测试用，不影响用户体验

use std::ffi::OsStr;

use tokio::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// `CREATE_NO_WINDOW` (0x08000000)：创建进程时不显示任何窗口
/// 参考：https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `CREATE_NEW_PROCESS_GROUP` (0x00000200)：创建新进程组
/// 用于：
/// 1. 让 Ctrl+C 信号只发给该进程组（不传给父进程）
/// 2. 与 CREATE_NO_WINDOW 配合使用
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// 创建不弹 cmd 窗口的 tokio Command
///
/// ## 跨平台行为
/// - **Windows**：自动加 `CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP` flag
/// - **Linux / macOS**：等价于 `Command::new`（POSIX 进程无"窗口"概念）
///
/// ## 使用示例
/// ```ignore
/// // 旧代码
/// let child = tokio::process::Command::new("uv")
///     .args(&["pip", "install", "torch"])
///     .output()
///     .await?;
///
/// // 新代码（仅替换 .new()）
/// use crate::common::process_util::new_command;
/// let output = new_command("uv")
///     .args(&["pip", "install", "torch"])
///     .output()
///     .await?;
/// ```
pub fn new_command<S: AsRef<OsStr>>(program: S) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_command_returns_command() {
        // 烟雾测试：返回类型必须是 tokio Command
        let _cmd: Command = new_command("echo");
    }

    #[test]
    fn test_new_command_accepts_path() {
        // 支持 PathBuf 等所有 AsRef<OsStr> 类型
        let path = std::path::PathBuf::from("C:\\Windows\\System32\\cmd.exe");
        let _cmd: Command = new_command(&path);
        let _cmd: Command = new_command(path);
    }
}
