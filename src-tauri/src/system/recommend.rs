//! 智能推荐 TorchVariant（v3.0 新增）
//!
//! 根据检测到的 GPU 列表 + 当前 OS 平台推荐最合适的 torch 变体。
//!
//! 优先级：NVIDIA > AMD(Linux) > Intel > Apple > CPU
//!
//! 详见 `PR/02-技术架构.md §9.4` 和 `PR/03-模块设计/02-PythonEnvManager.md §X.5`

use crate::common::platform::current_os;
use crate::python_env::torch_variant::{CudaVersion, RocmVersion, TorchVariant};

use super::gpu::{GpuInfo, GpuVendor};
use super::gpu_cache::get_or_detect;

/// 检测 GPU 并返回推荐变体（前端调用入口）
pub async fn recommend_torch_variant() -> TorchVariant {
    let gpus = get_or_detect().await;
    recommend_torch_variant_with_gpus(&gpus)
}

/// 给定 GPU 列表 + OS，推荐变体（纯函数，便于测试）
pub fn recommend_torch_variant_with_gpus(gpus: &[GpuInfo]) -> TorchVariant {
    let os = current_os();

    // 1. NVIDIA 优先
    if let Some(nvidia) = gpus.iter().find(|g| g.vendor == GpuVendor::Nvidia) {
        let cuda = nvidia
            .cuda_version
            .as_deref()
            .and_then(CudaVersion::from_driver_version)
            .unwrap_or(CudaVersion::V12_1);
        return TorchVariant::NvidiaCuda(cuda);
    }

    // 2. AMD ROCm（仅 Linux + Windows 实验性）
    if (os == "linux" || os == "windows")
        && gpus.iter().any(|g| g.vendor == GpuVendor::Amd)
    {
        return TorchVariant::AmdRocm(RocmVersion::V6_0);
    }

    // 3. Intel XPU
    if gpus.iter().any(|g| g.vendor == GpuVendor::Intel) {
        return TorchVariant::IntelXpu;
    }

    // 4. Apple Silicon
    if gpus.iter().any(|g| g.vendor == GpuVendor::Apple) {
        return TorchVariant::AppleSilicon;
    }

    // 5. 兜底 CPU
    TorchVariant::CpuOnly
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nvidia(cuda: Option<&str>) -> GpuInfo {
        GpuInfo {
            vendor: GpuVendor::Nvidia,
            model: "GeForce RTX 4080".to_string(),
            vram_mb: Some(16376),
            driver_version: Some("560.94".to_string()),
            cuda_version: cuda.map(|s| s.to_string()),
            rocm_version: None,
        }
    }

    fn amd() -> GpuInfo {
        GpuInfo {
            vendor: GpuVendor::Amd,
            model: "Radeon RX 7900 XT".to_string(),
            vram_mb: Some(20480),
            driver_version: None,
            cuda_version: None,
            rocm_version: None,
        }
    }

    fn intel() -> GpuInfo {
        GpuInfo {
            vendor: GpuVendor::Intel,
            model: "Intel Arc A770".to_string(),
            vram_mb: Some(16384),
            driver_version: None,
            cuda_version: None,
            rocm_version: None,
        }
    }

    fn apple() -> GpuInfo {
        GpuInfo {
            vendor: GpuVendor::Apple,
            model: "Apple M2 Pro".to_string(),
            vram_mb: Some(16384),
            driver_version: None,
            cuda_version: None,
            rocm_version: None,
        }
    }

    #[test]
    fn test_recommend_nvidia() {
        let gpus = vec![nvidia(Some("12.6"))];
        let v = recommend_torch_variant_with_gpus(&gpus);
        assert_eq!(v, TorchVariant::NvidiaCuda(CudaVersion::V12_4));
    }

    #[test]
    fn test_recommend_nvidia_no_cuda() {
        let gpus = vec![nvidia(None)];
        let v = recommend_torch_variant_with_gpus(&gpus);
        // 默认推荐 12.1
        assert_eq!(v, TorchVariant::NvidiaCuda(CudaVersion::V12_1));
    }

    #[test]
    fn test_recommend_fallback_cpu() {
        let gpus: Vec<GpuInfo> = vec![];
        let v = recommend_torch_variant_with_gpus(&gpus);
        assert_eq!(v, TorchVariant::CpuOnly);
    }

    #[test]
    fn test_recommend_priority_nvidia_first() {
        // 多 GPU 情况下，NVIDIA 优先
        let gpus = vec![amd(), nvidia(Some("12.6")), intel()];
        let v = recommend_torch_variant_with_gpus(&gpus);
        assert!(matches!(v, TorchVariant::NvidiaCuda(_)));
    }
}
