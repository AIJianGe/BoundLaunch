//! 统一错误类型定义
//!
//! 设计原则：
//! - 每个模块有自己的错误枚举（见各模块文档 §4.3）
//! - 通过 `#[from]` 自动转换
//! - 实现 `Serialize` 用于前端反序列化

use serde::{Serialize, Serializer};
use thiserror::Error;

/// 统一的应用错误类型（聚合所有模块错误）
///
/// 前端通过 invoke 收到 `Err(String)` 时反序列化为对应类型
#[derive(Debug, Error)]
pub enum AppError {
    #[error("Config 错误: {0}")]
    Config(#[from] ConfigError),

    #[error("环境错误: {0}")]
    Env(#[from] EnvError),

    #[error("ComfyUI 核心错误: {0}")]
    Core(#[from] CoreError),

    #[error("插件错误: {0}")]
    Plugin(#[from] PluginError),

    #[error("进程错误: {0}")]
    Process(#[from] ProcessError),

    #[error("模型路径错误: {0}")]
    ModelPath(#[from] ModelPathError),

    #[error("任务错误: {0}")]
    Task(#[from] TaskError),

    #[error("日志存储错误: {0}")]
    LogStore(String),
}

// Tauri command 返回 Result<T, String>，所以需要序列化为字符串
impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

#[derive(Debug, Error, Serialize)]
pub enum ConfigError {
    #[error("配置文件不存在: {0}")]
    NotFound(String),
    #[error("TOML 解析失败: {0}")]
    ParseError(String),
    #[error("TOML 序列化失败: {0}")]
    SerializeError(String),
    #[error("文件写入失败: {0}")]
    IoError(String),
    #[error("配置版本不兼容: 期望 {expected}, 实际 {actual}")]
    VersionMismatch { expected: u32, actual: u32 },
    #[error("字段值非法: {field} = {value}")]
    InvalidValue { field: String, value: String },
}

#[derive(Debug, Error, Serialize)]
pub enum EnvError {
    #[error("Python 安装失败: {0}")]
    PythonInstallFailed(String),
    #[error("uv 不存在或不可执行: {0}\n提示: 请安装 uv (https://docs.astral.sh/uv/) 或在设置中指定 uv 路径")]
    UvNotFound(String),
    #[error("venv 创建失败: {0}")]
    VenvCreateFailed(String),
    #[error("torch 安装失败: {0}")]
    TorchInstallFailed(String),
    #[error("requirements 安装失败: {0}")]
    RequirementsInstallFailed(String),
    #[error("ComfyUI 运行中，拒绝环境操作: {0}")]
    ComfyUIRunning(String),
    #[error("Python 切换失败（旧 venv 已恢复）: {detail}")]
    PythonSwitchFailed { detail: String },
    #[error("venv 重建失败: {detail}")]
    RebuildFailed { detail: String },
    #[error("verify_venv 失败: {0}")]
    VerifyFailed(String),
    /// v3.6：用户主动取消（CancellationToken 触发）
    #[error("操作已取消")]
    Cancelled,
    /// **v3.x Phase 2**：torch 安装失败 + 驱动/CUDA 不兼容（含 fallback 建议）
    ///
    /// 当 `install_torch` 失败时，**重新探测** GPU 算 fallback CUDA，
    /// 包装成结构化错误返回给前端。前端展示"是否切换到 {fallback} 重试？"。
    #[error("torch 安装失败 (尝试 {attempted}): {reason}")]
    TorchIncompatible {
        /// 用户尝试的 CUDA 版本
        attempted: String,
        /// 失败原因（pip/uv stderr 摘要）
        reason: String,
        /// 推荐的 fallback CUDA 版本（None = 没有合适的 fallback，建议改 CPU）
        fallback: Option<String>,
    },
}

#[derive(Debug, Error, Serialize)]
pub enum CoreError {
    #[error("ComfyUI 仓库未克隆")]
    NotCloned,
    #[error("ComfyUI 运行中，拒绝 checkout")]
    ComfyUIRunning,
    #[error("git 操作失败: {0}")]
    GitError(String),
    #[error("网络错误: {0}")]
    NetworkError(String),
    #[error("目录已存在: {0}")]
    AlreadyExists(std::path::PathBuf),
    /// 目标目录存在且非空，但不是 git 仓库（无法 clone）
    #[error("目录已存在但不是 git 仓库: {0}")]
    NotEmptyDir(std::path::PathBuf),
}

#[derive(Debug, Error, Serialize)]
pub enum PluginError {
    #[error("插件已存在: {0}")]
    AlreadyExists(String),
    #[error("插件不存在: {0}")]
    NotFound(String),
    #[error("git clone 失败: {0}")]
    CloneFailed(String),
    #[error("git pull 失败: {0}")]
    PullFailed(String),
    #[error("无效的 Git URL: {0}")]
    InvalidUrl(String),
}

#[derive(Debug, Error, Serialize)]
pub enum ProcessError {
    #[error("已有进程在运行 (PID: {pid})")]
    AlreadyRunning { pid: u32 },
    #[error("venv python 不存在: {0}")]
    PythonNotFound(String),
    #[error("ComfyUI main.py 不存在: {0}")]
    MainNotFound(String),
    #[error("子进程 spawn 失败: {0}")]
    SpawnFailed(String),
    #[error("端口 {port} 已被占用")]
    PortInUse { port: u16 },
    #[error("健康检查超时（{timeout}s）")]
    HealthCheckTimeout { timeout: u64 },
    #[error("进程未运行")]
    NotRunning,
    #[error("停止失败：进程拒绝退出")]
    StopFailed,
    #[error("日志流读取失败: {0}")]
    LogStreamError(String),
    #[error("环境未就绪: {detail}")]
    EnvironmentNotReady { detail: String },
    #[error("环境脏状态: {detail}")]
    DirtyState { detail: String },
    #[error("IO 错误: {0}")]
    Io(String),
    #[error("进程已退出")]
    ProcessExited,
    /// v3.4：spawn 后早期退出检测窗口（默认 5s）内 child 死亡
    ///
    /// 触发场景：ComfyUI 启动阶段即崩溃（ImportError / 参数错误 / 端口被占 / main.py 顶层异常 等）。
    /// 载荷含 `exit_code` 和 `stderr_tail`（最近 50 行日志），前端可一次性看到 Python 报错全貌。
    /// 与 `HealthCheckTimeout` 的区别：早期窗口 = spawn 失败，60s 健康检查 = 进程能跑但接口不响应。
    #[error("ComfyUI 启动后 {window_secs}s 内退出 (exit code: {exit_code:?})\n\n最近日志：\n{stderr_tail}")]
    EarlyExit {
        exit_code: Option<i32>,
        stderr_tail: String,
        window_secs: u64,
    },
}

impl From<std::io::Error> for ProcessError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

impl From<crate::error::EnvError> for ProcessError {
    fn from(e: crate::error::EnvError) -> Self {
        Self::EnvironmentNotReady {
            detail: e.to_string(),
        }
    }
}

#[derive(Debug, Error, Serialize)]
pub enum ModelPathError {
    #[error("根目录不存在: {0}")]
    RootNotFound(String),
    #[error("根目录不可写: {0}")]
    RootNotWritable(String),
    #[error("yaml 生成失败: {0}")]
    YamlGenFailed(String),
    #[error("子目录创建失败: {0}")]
    SubdirCreateFailed(String),
}

#[derive(Debug, Error, Serialize)]
pub enum TaskError {
    #[error("任务不存在: {0}")]
    NotFound(String),
    #[error("任务已取消: {0}")]
    Cancelled(String),
    #[error("任务执行失败: {0}")]
    ExecutionFailed(String),
    #[error("任务超时")]
    Timeout,
}

/// 通用 Result 别名
pub type AppResult<T> = Result<T, AppError>;

/// v3.6：SubprocessError → EnvError 转换
///
/// `Cancelled` → `EnvError::Cancelled`（而非 `VerifyFailed`，语义更清晰）
impl From<crate::common::subprocess::SubprocessError> for EnvError {
    fn from(e: crate::common::subprocess::SubprocessError) -> Self {
        match e {
            crate::common::subprocess::SubprocessError::Cancelled => EnvError::Cancelled,
            crate::common::subprocess::SubprocessError::Io(e) => {
                EnvError::VerifyFailed(e.to_string())
            }
            crate::common::subprocess::SubprocessError::Exit { code, stderr } => {
                EnvError::VerifyFailed(format!("exit {}: {}", code, stderr))
            }
        }
    }
}
