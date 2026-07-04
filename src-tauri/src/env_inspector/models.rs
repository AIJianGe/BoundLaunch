//! EnvironmentInspector 值对象
//!
//! 详见 `PR/03-模块设计/07-EnvironmentInspector.md §3 接口签名`

use chrono::{DateTime, Utc};
use serde::Serialize;

/// 完整环境信息（前端顶部状态卡片 + 关键依赖列表用）
#[derive(Debug, Clone, Serialize)]
pub struct EnvInfo {
    pub torch: TorchInfo,
    pub dependencies: Vec<DependencyInfo>,
    pub gpu: GpuInfo,
    /// ComfyUI 当前版本（来自 CoreManager，Phase 7+ 后填充）
    pub comfyui_version: Option<String>,
    /// 当前生效的启动参数（来自 ProcessLauncher，Phase 11+ 后填充）
    pub running_args: Option<Vec<(String, Option<String>)>>,
    pub inspected_at: DateTime<Utc>,
}

/// torch 安装与 CUDA 信息
#[derive(Debug, Clone, Serialize)]
pub struct TorchInfo {
    pub installed: bool,
    pub version: Option<String>,
    pub cuda_available: bool,
    pub cuda_version: Option<String>,
    pub device_name: Option<String>,
    pub device_count: u32,
    pub total_memory_mb: Option<u64>,
}

impl TorchInfo {
    /// torch 未安装时的默认值
    pub fn not_installed() -> Self {
        Self {
            installed: false,
            version: None,
            cuda_available: false,
            cuda_version: None,
            device_name: None,
            device_count: 0,
            total_memory_mb: None,
        }
    }
}

/// 单个依赖的版本与状态
#[derive(Debug, Clone, Serialize)]
pub struct DependencyInfo {
    pub name: String,
    pub installed_version: Option<String>,
    /// 来自 ComfyUI requirements.txt 的版本约束
    pub required_version: Option<String>,
    pub status: DepStatus,
}

/// 依赖状态
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", content = "detail")]
pub enum DepStatus {
    /// 已安装且满足版本要求
    Satisfied,
    /// 已安装但版本低于要求
    NeedsUpgrade { current: String, required: String },
    /// 未安装
    Missing,
    /// requirements.txt 中未约束此包
    NotRequired,
}

/// GPU 设备信息
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "vendor", content = "info")]
pub enum GpuInfo {
    Nvidia {
        name: String,
        memory_mb: u64,
        driver_version: String,
    },
    Amd { name: String },
    Intel { name: String },
    CpuOnly { cpu_model: String },
    Unknown,
}

impl Default for GpuInfo {
    fn default() -> Self {
        Self::Unknown
    }
}
