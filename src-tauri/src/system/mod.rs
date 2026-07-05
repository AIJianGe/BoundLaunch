//! System 模块（v3.0 新增）
//!
//! 跨平台硬件检测 + 智能推荐。
//!
//! - `gpu`：GPU 自动检测（NVIDIA / AMD / Intel / Apple）
//! - `gpu_cache`：5 分钟结果缓存
//! - `recommend`：根据检测结果推荐 TorchVariant
//!
//! 详见 `PR/03-模块设计/02-PythonEnvManager.md §X` 和 `PR/02-技术架构.md §9`

pub mod gpu;
pub mod gpu_cache;
pub mod recommend;

pub use gpu::{detect_gpus, GpuInfo, GpuVendor};
pub use gpu_cache::{clear_gpu_cache, detect_and_cache, get_cached_gpus, get_or_detect};
pub use recommend::{recommend_torch_variant, recommend_torch_variant_with_gpus};
