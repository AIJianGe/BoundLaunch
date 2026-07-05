//! ProcessLauncher 数据模型
//!
//! 详见 `PR/03-模块设计/06-ProcessLauncher.md §3 接口签名` 与 `§4.3 错误类型`
//!
//! 设计要点：
//! - 复用 `crate::config` 中的 `LaunchMode` / `PreviewMethod` / `AdvancedArgs`，避免重复定义
//! - 复用 `crate::error::ProcessError` 作为模块错误类型（已含 `From<io::Error>` / `From<EnvError>`）
//! - `LaunchArgs` 是运行时参数快照（从 Config 转换得到，运行期间不可变）
//! - `ProcessStatus` 是状态机值对象，状态转换规则在 `state_machine.rs`

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// 复用 Config 中的类型，减少冗余
pub use crate::config::{AdvancedArgs, LaunchMode, PreviewMethod};
// 复用统一错误类型

/// 进程启动参数（运行时快照）
///
/// 由 `Config.launch` 转换得到，运行期间不可变。
/// 与 Config 解耦：Config 变更不影响已启动的进程。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchArgs {
    /// 显存策略 / 运行模式
    pub mode: LaunchMode,
    /// 监听地址（如 "127.0.0.1" / "0.0.0.0"）
    pub listen_host: String,
    /// 监听端口
    pub listen_port: u16,
    /// 预览方式
    pub preview_method: PreviewMethod,
    /// 启动后自动打开浏览器（对应 Config 的 auto_open_browser）
    pub auto_launch: bool,
    /// 高级参数
    pub advanced: AdvancedArgs,
    /// 自定义启动参数（仅 `LaunchMode::Custom` 时使用，空字符串视为 None）
    pub custom_args: Option<String>,
}

impl LaunchArgs {
    /// 默认值（与 `Config::default().launch` 对齐）
    pub fn defaults() -> Self {
        Self {
            mode: LaunchMode::GpuHigh,
            listen_host: "127.0.0.1".into(),
            listen_port: 8188,
            preview_method: PreviewMethod::Latent,
            auto_launch: true,
            advanced: AdvancedArgs::default(),
            custom_args: None,
        }
    }
}

/// 进程状态机
///
/// 转换规则：见 `state_machine.rs`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProcessStatus {
    /// 已停止（初始态 / 终态）
    Stopped,
    /// 启动中（已 spawn，健康检查未通过）
    Starting {
        /// spawn 时间
        started_at: DateTime<Utc>,
        /// 预期监听端口（健康检查用）
        port: u16,
    },
    /// 运行中（健康检查通过）
    Running {
        /// 子进程 PID
        pid: u32,
        /// spawn 时间
        started_at: DateTime<Utc>,
        /// 实际监听端口
        port: u16,
    },
    /// 停止中（已发 SIGTERM，等待退出）
    Stopping {
        /// 停止原因
        reason: StopReason,
    },
    /// 崩溃（异常退出）
    Crashed {
        /// 退出码（None 表示被信号杀死）
        exit_code: Option<i32>,
        /// 错误描述
        error: String,
        /// 崩溃时间
        at: DateTime<Utc>,
    },
}

impl ProcessStatus {
    /// 是否处于终态（不会再发生状态转换）
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Stopped | Self::Crashed { .. })
    }

    /// 是否处于运行中或启动中
    pub fn is_alive(&self) -> bool {
        matches!(self, Self::Starting { .. } | Self::Running { .. })
    }

    /// 是否正在运行（健康检查已通过）
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }

    /// 序列化用字符串标签
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Starting { .. } => "starting",
            Self::Running { .. } => "running",
            Self::Stopping { .. } => "stopping",
            Self::Crashed { .. } => "crashed",
        }
    }
}

impl Default for ProcessStatus {
    fn default() -> Self {
        Self::Stopped
    }
}

/// 停止原因
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// 用户主动停止
    UserRequested,
    /// 健康检查超时
    HealthCheckTimeout,
    /// 外部信号（如操作系统关闭）
    ExternalSignal,
    /// 父进程退出
    ParentExit,
    /// F24 退出流程触发（由 ShutdownCoordinator 调用 stop 传此 reason）
    ///
    /// 与 UserRequested 的区别：会联动清理更多资源（关闭进程组、广播 AppExited）
    Shutdown,
}

impl StopReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UserRequested => "user_requested",
            Self::HealthCheckTimeout => "health_check_timeout",
            Self::ExternalSignal => "external_signal",
            Self::ParentExit => "parent_exit",
            Self::Shutdown => "shutdown",
        }
    }
}

/// F24 退出流程结果报告（由 ShutdownCoordinator 5 步事务完成后产出）
///
/// 前端调用 `invoke('shutdown_all')` 时接收，载荷含 ComfyUI 运行时状态、
/// 实际停止耗时、退出原因，便于审计 / 日志 / 遥测。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShutdownReport {
    /// ComfyUI 是否在退出前处于运行中
    ///
    /// 由 ShutdownCoordinator 在 stop 之前查询 `process_launcher.is_running()` 决定。
    /// false 表示未启动 ComfyUI 直接退出（无需 stop 流程）。
    pub comfyui_was_running: bool,

    /// ComfyUI 停止阶段耗时（毫秒）
    ///
    /// - comfyui_was_running=false：固定为 0
    /// - comfyui_was_running=true：从 `process_launcher.stop()` 开始到 `child.wait()` 完成
    pub stop_elapsed_ms: u64,

    /// 退出原因（与 AppExiting 事件载荷对齐）
    pub reason: crate::event_bus::ShutdownReason,
}

/// 健康检查结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthInfo {
    /// 是否就绪
    pub ready: bool,
    /// HTTP 状态码（如有响应）
    pub status_code: Option<u16>,
    /// 响应耗时（毫秒）
    pub elapsed_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_is_terminal() {
        assert!(ProcessStatus::Stopped.is_terminal());
        assert!(ProcessStatus::Crashed {
            exit_code: Some(1),
            error: "boom".into(),
            at: Utc::now(),
        }
        .is_terminal());
        assert!(!ProcessStatus::Starting {
            started_at: Utc::now(),
            port: 8188,
        }
        .is_terminal());
    }

    #[test]
    fn test_status_is_alive() {
        assert!(ProcessStatus::Starting {
            started_at: Utc::now(),
            port: 8188,
        }
        .is_alive());
        assert!(ProcessStatus::Running {
            pid: 1234,
            started_at: Utc::now(),
            port: 8188,
        }
        .is_alive());
        assert!(!ProcessStatus::Stopped.is_alive());
        assert!(!ProcessStatus::Crashed {
            exit_code: Some(1),
            error: "".into(),
            at: Utc::now(),
        }
        .is_alive());
    }

    #[test]
    fn test_status_as_str() {
        assert_eq!(ProcessStatus::Stopped.as_str(), "stopped");
        assert_eq!(
            ProcessStatus::Running {
                pid: 1,
                started_at: Utc::now(),
                port: 8188,
            }
            .as_str(),
            "running"
        );
    }

    #[test]
    fn test_launch_args_defaults() {
        let args = LaunchArgs::defaults();
        assert_eq!(args.listen_port, 8188);
        assert_eq!(args.listen_host, "127.0.0.1");
        assert!(args.auto_launch);
        assert!(args.custom_args.is_none());
    }
}
