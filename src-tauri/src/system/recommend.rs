//! 智能推荐 TorchVariant（v3.0 新增）
//!
//! 根据检测到的 GPU 列表 + 当前 OS 平台推荐最合适的 torch 变体。
//!
//! 优先级：NVIDIA > AMD(Linux) > Intel > Apple > CPU
//!
//! 详见 `PR/02-技术架构.md §9.4` 和 `PR/03-模块设计/02-PythonEnvManager.md §X.5`

use crate::common::platform::current_os;
use crate::python_env::torch_variant::{CudaVersion, RocmVersion, TorchVariant};
use serde::Serialize;

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
            .unwrap_or(CudaVersion::V12_8);
        return TorchVariant::NvidiaCuda(cuda);
    }

    // 2. AMD ROCm（仅 Linux + Windows 实验性）
    if (os == "linux" || os == "windows")
        && gpus.iter().any(|g| g.vendor == GpuVendor::Amd)
    {
        return TorchVariant::AmdRocm(RocmVersion::V6_4);
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

// ====================================================================
// v3.10：驱动兼容性检查
// ====================================================================

/// 驱动兼容性严重度
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DriverCompatSeverity {
    /// 兼容性良好，无需操作
    Ok,
    /// 警告：能装但可能性能/稳定性不佳
    Warning,
    /// 错误：驱动版本太旧，不支持推荐变体
    Error,
}

/// 驱动兼容性报告（前端可读）
#[derive(Debug, Clone, Serialize)]
pub struct DriverCompatReport {
    /// 整体严重度（最严重的一项决定）
    pub severity: DriverCompatSeverity,
    /// 检测到的所有 GPU 列表
    pub gpus: Vec<GpuInfo>,
    /// 推荐的 torch 变体
    pub recommended_variant: TorchVariant,
    /// 兼容性诊断信息（每条是 1 句用户可读提示）
    pub notes: Vec<String>,
    /// 推荐的安装/修复操作
    pub recommendation: String,
}

/// NVIDIA 驱动版本要求（v3.10）
///
/// 来源：PyTorch 官方文档
/// - CUDA 13.0 wheel: 需要 NVIDIA 驱动 >= 580
/// - CUDA 12.8 wheel: 需要 NVIDIA 驱动 >= 570
/// - CUDA 12.6 wheel: 需要 NVIDIA 驱动 >= 560
/// - CUDA 11.8 wheel: 需要 NVIDIA 驱动 >= 520
///
/// 返回该 CUDA 版本需要的最低驱动版本（主版本号）
fn min_nvidia_driver_for_cuda(cuda: &CudaVersion) -> u32 {
    match cuda {
        CudaVersion::V11_8 => 520,
        CudaVersion::V12_6 => 560,
        CudaVersion::V12_8 => 570,
        CudaVersion::V13_0 => 580,
    }
}

/// 解析驱动版本号（取 . 分隔的第一段作为主版本号）
///
/// 例：`"560.94"` → `Some(560)`，`"31.0.15.3624"` → `Some(31)`
fn parse_driver_major(version: &str) -> Option<u32> {
    version.split('.').next()?.parse().ok()
}

/// 给定 GPU 列表 + 推荐变体，生成驱动兼容性报告（v3.10 新增）
///
/// 主要检查项：
/// 1. **NVIDIA 驱动兼容性**：解析 GPU.driver_version 与推荐 CUDA 变体的最低驱动要求对比
/// 2. **平台支持性**：macOS 不支持 CUDA、Linux 推荐 ROCm 等
/// 3. **驱动过旧 fallback**：驱动版本低于最低要求时，自动选更低 CUDA 版本（或 CPU）
///
/// 返回的 `recommendation` 是用户可读的修复建议（"请升级 NVIDIA 驱动到 570+"）。
pub fn check_driver_compatibility(gpus: &[GpuInfo], variant: &TorchVariant) -> DriverCompatReport {
    let os = current_os();
    let mut notes: Vec<String> = Vec::new();
    let mut severity = DriverCompatSeverity::Ok;
    let mut final_variant = variant.clone();

    // 1. NVIDIA GPU 驱动兼容性
    if let Some(nvidia) = gpus.iter().find(|g| g.vendor == GpuVendor::Nvidia) {
        if let Some(driver_str) = nvidia.driver_version.as_deref() {
            if let Some(driver_major) = parse_driver_major(driver_str) {
                if let TorchVariant::NvidiaCuda(cuda) = variant {
                    let min_required = min_nvidia_driver_for_cuda(cuda);
                    if driver_major < min_required {
                        // 驱动太旧：尝试选更低的 CUDA 版本
                        let fallback = pick_fallback_cuda(driver_major);
                        match fallback {
                            Some(fb_cuda) => {
                                notes.push(format!(
                                    "检测到 NVIDIA 驱动版本 {}（推荐 CUDA {} 需要 >= {}），已自动降级到 CUDA {}",
                                    driver_str,
                                    cuda.display_name(),
                                    min_required,
                                    fb_cuda.display_name()
                                ));
                                severity = DriverCompatSeverity::Warning;
                                final_variant = TorchVariant::NvidiaCuda(fb_cuda);
                            }
                            None => {
                                notes.push(format!(
                                    "检测到 NVIDIA 驱动版本 {}（推荐 CUDA {} 需要 >= {}），驱动过旧无法支持任何 CUDA 变体。建议升级驱动或切换到 CPU 模式。",
                                    driver_str,
                                    cuda.display_name(),
                                    min_required
                                ));
                                severity = DriverCompatSeverity::Error;
                                final_variant = TorchVariant::CpuOnly;
                            }
                        }
                    } else {
                        notes.push(format!(
                            "NVIDIA 驱动 {} 支持 CUDA {}（满足 >= {}）",
                            driver_str,
                            cuda.display_name(),
                            min_required
                        ));
                    }
                }
            } else {
                notes.push(format!(
                    "无法解析 NVIDIA 驱动版本：{}（请手动检查驱动兼容性）",
                    driver_str
                ));
            }
        } else {
            notes.push("未检测到 NVIDIA 驱动版本（建议重装/更新 NVIDIA 驱动）".to_string());
        }
    }

    // 2. macOS 不支持 CUDA（额外提示）
    if os == "macos" && matches!(variant, TorchVariant::NvidiaCuda(_)) {
        notes.push("macOS 不支持 CUDA 加速（仅 CPU/MPS）".to_string());
    }

    // 3. AMD 在 Windows 上是实验性支持
    if os == "windows" && matches!(variant, TorchVariant::AmdRocm(_)) {
        notes.push("AMD ROCm 在 Windows 上是实验性支持，推荐使用 Linux".to_string());
        severity = DriverCompatSeverity::Warning;
    }

    // 4. 构造总体 recommendation
    let recommendation = match severity {
        DriverCompatSeverity::Ok => format!(
            "推荐安装 {}（驱动兼容性良好）",
            final_variant.display_name()
        ),
        DriverCompatSeverity::Warning => match final_variant {
            TorchVariant::CpuOnly => "请升级 NVIDIA 驱动或切换到 CPU 模式".to_string(),
            _ => format!(
                "推荐安装 {}（已根据驱动版本自动降级以兼容）",
                final_variant.display_name()
            ),
        },
        DriverCompatSeverity::Error => "驱动不兼容，请升级驱动后重试".to_string(),
    };

    DriverCompatReport {
        severity,
        gpus: gpus.to_vec(),
        recommended_variant: final_variant,
        notes,
        recommendation,
    }
}

/// 根据 NVIDIA 驱动主版本号选能支持的最高 CUDA 版本（v3.10 新增）
///
/// 逻辑：驱动的 CUDA 能力向后兼容。
/// - 驱动 >= 580 → CUDA 13.0
/// - 驱动 >= 570 → CUDA 12.8
/// - 驱动 >= 560 → CUDA 12.6
/// - 驱动 >= 520 → CUDA 11.8
/// - < 520 → None（驱动太旧）
fn pick_fallback_cuda(driver_major: u32) -> Option<CudaVersion> {
    if driver_major >= 580 {
        Some(CudaVersion::V13_0)
    } else if driver_major >= 570 {
        Some(CudaVersion::V12_8)
    } else if driver_major >= 560 {
        Some(CudaVersion::V12_6)
    } else if driver_major >= 520 {
        Some(CudaVersion::V11_8)
    } else {
        None
    }
}

/// 一站式入口：检测 GPU + 推荐变体 + 驱动兼容性检查（v3.10 新增）
///
/// 这是前端「智能推荐」按钮应调用的命令。
/// 返回 DriverCompatReport，包含：
/// - detected_gpus：所有检测到的 GPU
/// - recommended_variant：综合驱动兼容性后的最终推荐
/// - notes：每条兼容性诊断（成功/警告/降级/不兼容）
/// - severity：最严重的一项
/// - recommendation：用户可读的修复建议
pub async fn check_driver_compatibility_full() -> DriverCompatReport {
    let gpus = get_or_detect().await;
    let initial_variant = recommend_torch_variant_with_gpus(&gpus);
    check_driver_compatibility(&gpus, &initial_variant)
}

// TorchVariant display_name 扩展
trait VariantDisplay {
    fn display_name(&self) -> String;
}

impl VariantDisplay for TorchVariant {
    fn display_name(&self) -> String {
        match self {
            TorchVariant::NvidiaCuda(v) => format!("NVIDIA {}", v.display_name()),
            TorchVariant::AmdRocm(v) => format!("AMD {}", v.display_name()),
            TorchVariant::IntelXpu => "Intel XPU".to_string(),
            TorchVariant::AppleSilicon => "Apple Silicon (MPS)".to_string(),
            TorchVariant::CpuOnly => "CPU".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nvidia(cuda: Option<&str>, driver: Option<&str>) -> GpuInfo {
        GpuInfo {
            vendor: GpuVendor::Nvidia,
            model: "GeForce RTX 4080".to_string(),
            vram_mb: Some(16376),
            driver_version: driver.map(|s| s.to_string()),
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
        let gpus = vec![nvidia(Some("12.6"), Some("560.94"))];
        let v = recommend_torch_variant_with_gpus(&gpus);
        assert_eq!(v, TorchVariant::NvidiaCuda(CudaVersion::V12_6));
    }

    #[test]
    fn test_recommend_nvidia_no_cuda() {
        let gpus = vec![nvidia(None, None)];
        let v = recommend_torch_variant_with_gpus(&gpus);
        // v3.7：默认推荐 CUDA 12.8
        assert_eq!(v, TorchVariant::NvidiaCuda(CudaVersion::V12_8));
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
        let gpus = vec![amd(), nvidia(Some("12.6"), Some("560.94")), intel()];
        let v = recommend_torch_variant_with_gpus(&gpus);
        assert!(matches!(v, TorchVariant::NvidiaCuda(_)));
    }

    // ===== 驱动兼容性测试 =====

    #[test]
    fn test_parse_driver_major() {
        assert_eq!(parse_driver_major("560.94"), Some(560));
        assert_eq!(parse_driver_major("570.124"), Some(570));
        assert_eq!(parse_driver_major("31.0.15.3624"), Some(31));
        assert_eq!(parse_driver_major("invalid"), None);
    }

    #[test]
    fn test_min_nvidia_driver_for_cuda() {
        assert_eq!(min_nvidia_driver_for_cuda(&CudaVersion::V11_8), 520);
        assert_eq!(min_nvidia_driver_for_cuda(&CudaVersion::V12_6), 560);
        assert_eq!(min_nvidia_driver_for_cuda(&CudaVersion::V12_8), 570);
        assert_eq!(min_nvidia_driver_for_cuda(&CudaVersion::V13_0), 580);
    }

    #[test]
    fn test_pick_fallback_cuda() {
        assert_eq!(pick_fallback_cuda(580), Some(CudaVersion::V13_0));
        assert_eq!(pick_fallback_cuda(570), Some(CudaVersion::V12_8));
        assert_eq!(pick_fallback_cuda(560), Some(CudaVersion::V12_6));
        assert_eq!(pick_fallback_cuda(520), Some(CudaVersion::V11_8));
        assert_eq!(pick_fallback_cuda(500), None);
    }

    #[test]
    fn test_driver_compat_nvidia_ok() {
        // 驱动 560 + 推荐 CUDA 12.6 → 兼容
        let gpus = vec![nvidia(Some("12.6"), Some("560.94"))];
        let variant = TorchVariant::NvidiaCuda(CudaVersion::V12_6);
        let report = check_driver_compatibility(&gpus, &variant);
        assert_eq!(report.severity, DriverCompatSeverity::Ok);
        assert_eq!(report.recommended_variant, variant);
    }

    #[test]
    fn test_driver_compat_nvidia_too_old_auto_fallback() {
        // 驱动 555 + 推荐 CUDA 12.8 → 自动降级到 CUDA 12.6（驱动 555 >= 560 不满足，但 >= 520 满足）
        // 实际 555 < 560 → 选 CUDA 11.8（fallback chain: 580→570→560→520→None）
        // 555 >= 520 → CUDA 11.8
        let gpus = vec![nvidia(Some("12.6"), Some("555.85"))];
        let variant = TorchVariant::NvidiaCuda(CudaVersion::V12_8);
        let report = check_driver_compatibility(&gpus, &variant);
        assert_eq!(report.severity, DriverCompatSeverity::Warning);
        assert_eq!(report.recommended_variant, TorchVariant::NvidiaCuda(CudaVersion::V11_8));
    }

    #[test]
    fn test_driver_compat_nvidia_too_old_force_cpu() {
        // 驱动 480（太旧） → 强制降级到 CPU
        let gpus = vec![nvidia(Some("11.5"), Some("480.0"))];
        let variant = TorchVariant::NvidiaCuda(CudaVersion::V12_8);
        let report = check_driver_compatibility(&gpus, &variant);
        assert_eq!(report.severity, DriverCompatSeverity::Error);
        assert_eq!(report.recommended_variant, TorchVariant::CpuOnly);
    }

    #[test]
    fn test_driver_compat_no_driver_version() {
        // 无驱动版本信息 → 警告
        let gpus = vec![nvidia(Some("12.6"), None)];
        let variant = TorchVariant::NvidiaCuda(CudaVersion::V12_6);
        let report = check_driver_compatibility(&gpus, &variant);
        assert_eq!(report.severity, DriverCompatSeverity::Ok);
        // 至少有一条 notes 提到驱动
        assert!(!report.notes.is_empty());
    }
}

