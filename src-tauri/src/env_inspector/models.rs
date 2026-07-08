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

/// 扁平环境快照（v2.13 前端 `env_inspect` 命令返回）
///
/// v2.10 之前 `env_inspect` 直接返回 `EnvInfo`（嵌套结构），
/// 但前端 `EnvInfo` 类型假设的是扁平字段（`torch_installed` / `comfyui_cloned` /
/// `last_updated` 等），导致前端读不到数据 → 显示「未安装 / 未配置」。
///
/// 改为返回本结构，扁平字段 + 直接对应前端类型：
/// - `StatusCard.vue` 读 `envInfo.torch_installed / cuda_version / gpu_name` 等
/// - 关键依赖列表读 `envInfo.dependencies`
/// - 页脚读 `envInfo.venv_path / comfyui_cloned / last_updated`
///
/// 保留 `EnvInfo` 嵌套结构供模块内部其他消费者使用。
#[derive(Debug, Clone, Serialize)]
pub struct EnvSnapshot {
    // —— 路径 ——
    pub python_path: String,
    pub venv_path: String,
    pub comfyui_root: String,

    // —— Python / Torch 状态 ——
    pub python_version: String,
    pub torch_installed: bool,
    pub torch_version: Option<String>,
    pub torchvision_installed: bool,
    pub torchvision_ops_available: bool,
    pub torchvision_io_available: bool,
    pub cuda_available: bool,
    pub cuda_version: Option<String>,
    pub gpu_name: Option<String>,

    // —— ComfyUI ——
    pub comfyui_cloned: bool,

    // —— 依赖 ——
    pub dependencies: Vec<DependencyInfo>,

    // —— 元信息 ——
    pub last_updated: DateTime<Utc>,
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
    pub torchvision: TorchvisionInfo,
}

/// torchvision 完整性信息
#[derive(Debug, Clone, Serialize)]
pub struct TorchvisionInfo {
    pub installed: bool,
    pub version: Option<String>,
    pub ops_available: bool,
    pub io_available: bool,
}

impl Default for TorchvisionInfo {
    fn default() -> Self {
        Self {
            installed: false,
            version: None,
            ops_available: false,
            io_available: false,
        }
    }
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
            torchvision: TorchvisionInfo::default(),
        }
    }
}

/// 单个依赖的版本与状态
#[derive(Debug, Clone, Serialize)]
pub struct DependencyInfo {
    pub name: String,
    /// 实际安装的版本（前端用 `version` 字段名访问）
    #[serde(rename = "version")]
    pub installed_version: Option<String>,
    /// 来自 ComfyUI requirements.txt 的版本约束
    pub required_version: Option<String>,
    pub status: DepStatus,
}

/// 依赖状态
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
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
