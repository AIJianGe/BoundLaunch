//! PythonEnvManager 值对象
//!
//! 详见 `PR/03-模块设计/02-PythonEnvManager.md §3 接口签名`

use std::path::PathBuf;
use serde::Serialize;

/// 环境状态快照
#[derive(Debug, Clone, Serialize)]
pub struct EnvInfo {
    pub python_version: String,
    pub python_path: PathBuf,
    pub venv_path: PathBuf,
    pub torch_installed: bool,
    pub torch_version: Option<String>,
    pub cuda_available: bool,
}

/// Python 环境状态总览（v2.13）
///
/// 前端 `PythonEnvStatus` 类型对应的后端结构。
/// 通过 `env_status` 命令返回，供：
/// - 设置页「Python 版本切换」面板的「当前版本」显示
/// - 首页「PYTHON」卡片
/// - readiness 检查
///
/// 之前 v2.10 之前 `env_status` 返回的是 `EnvInfo`（不含 `venv_python_version` 等字段），
/// 导致前端读不到数据 → 显示「未配置」。
/// 实际数据全部能从文件系统探测出来（venv 目录存在 → 跑 `python -c "import sys; print(sys.version)"`）。
#[derive(Debug, Clone, Serialize)]
pub struct PythonEnvStatus {
    /// uv 是否已安装且可执行
    pub uv_installed: bool,
    /// uv binary 绝对路径
    pub uv_path: Option<String>,
    /// uv 版本号（uv --version 输出）
    pub uv_version: Option<String>,
    /// venv 目录是否存在
    pub venv_exists: bool,
    /// venv 中的 Python 版本（如 "3.11.10"）
    pub venv_python_version: Option<String>,
    /// venv 中是否已安装 torch
    pub venv_torch_installed: bool,
    /// venv 中 torch 版本号
    pub venv_torch_version: Option<String>,
    /// venv 中 torch CUDA 是否可用
    pub venv_torch_cuda: bool,
}

impl PythonEnvStatus {
    /// 默认空状态（uv/venv 都不存在时）
    pub fn empty() -> Self {
        Self {
            uv_installed: false,
            uv_path: None,
            uv_version: None,
            venv_exists: false,
            venv_python_version: None,
            venv_torch_installed: false,
            venv_torch_version: None,
            venv_torch_cuda: false,
        }
    }
}

/// 安装进度回调
#[derive(Debug, Clone, Serialize)]
pub struct InstallProgress {
    pub stage: InstallStage,
    pub message: String,
    pub percent: Option<u8>,
}

/// 安装阶段
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
pub enum InstallStage {
    DownloadingPython,
    CreatingVenv,
    InstallingTorch,
    InstallingRequirements,
    Verifying,
    Done,
    Failed(String),
}

impl InstallStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DownloadingPython => "downloading_python",
            Self::CreatingVenv => "creating_venv",
            Self::InstallingTorch => "installing_torch",
            Self::InstallingRequirements => "installing_requirements",
            Self::Verifying => "verifying",
            Self::Done => "done",
            Self::Failed(_) => "failed",
        }
    }
}

/// 依赖兼容性报告
#[derive(Debug, Clone, Serialize, Default)]
pub struct CompatibilityReport {
    /// requirements.txt 要求但 venv 未装的包
    pub missing: Vec<PackageReq>,
    /// 装了但版本不满足的包
    pub outdated: Vec<PackageMismatch>,
    /// missing + outdated 都为空时 true
    pub is_compatible: bool,
}

/// 单个缺失包的需求
#[derive(Debug, Clone, Serialize)]
pub struct PackageReq {
    pub name: String,
    pub required_version: String,
}

/// 已装包版本不匹配
#[derive(Debug, Clone, Serialize)]
pub struct PackageMismatch {
    pub name: String,
    pub required_version: String,
    pub installed_version: String,
}

/// 系统检测到的可用 Python
#[derive(Debug, Clone, Serialize)]
pub struct PythonInfo {
    pub version: String,
    pub path: PathBuf,
}
