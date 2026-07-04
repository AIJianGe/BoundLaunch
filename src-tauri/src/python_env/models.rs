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
