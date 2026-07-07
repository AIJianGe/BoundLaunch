//! torch 安装变体抽象（v3.0 新增，v3.7 更新版本列表）
//!
//! 支持 5 厂商：NVIDIA CUDA / AMD ROCm / Intel XPU / Apple Silicon MPS / CPU
//!
//! 每个变体统一通过 `install_args` 返回 uv pip install 参数，
//! 通过 `verify_command` 返回安装后验证命令。
//!
//! v3.7 版本更新（2025-12，对齐 PyTorch 2.11 官方 wheel）：
//! - NVIDIA CUDA: 删除已弃用的 cu121/cu124，新增 cu126/cu128/cu130
//! - AMD ROCm: 删除过时的 rocm5.7/6.0/6.1，新增 rocm6.3/6.4/7.0/7.1/7.2
//!
//! 详见 `PR/03-模块设计/02-PythonEnvManager.md §X`

use serde::{Deserialize, Serialize};

/// torch 安装变体（v3.0 新增）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "vendor", content = "version", rename_all = "snake_case")]
pub enum TorchVariant {
    /// NVIDIA CUDA（Windows / Linux）
    NvidiaCuda(CudaVersion),
    /// AMD ROCm（仅 Linux，Windows 实验性）
    AmdRocm(RocmVersion),
    /// Intel XPU（Windows 11+ / Linux）
    IntelXpu,
    /// Apple Silicon MPS（仅 macOS，使用 CPU 版 torch + MPS 内置）
    AppleSilicon,
    /// CPU only
    CpuOnly,
}

/// CUDA 版本
///
/// v3.7：对齐 PyTorch 2.11 官方 wheel
/// - 删除 cu121 / cu124（PyTorch 2.9+ 不再提供）
/// - 新增 cu126 / cu128 / cu130
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum CudaVersion {
    #[serde(rename = "cu118")]
    V11_8,
    #[serde(rename = "cu126")]
    V12_6,
    #[serde(rename = "cu128")]
    V12_8,
    #[serde(rename = "cu130")]
    V13_0,
}

impl CudaVersion {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::V11_8 => "cu118",
            Self::V12_6 => "cu126",
            Self::V12_8 => "cu128",
            Self::V13_0 => "cu130",
        }
    }

    /// 显示名（如 "CUDA 12.8"）
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::V11_8 => "CUDA 11.8",
            Self::V12_6 => "CUDA 12.6",
            Self::V12_8 => "CUDA 12.8",
            Self::V13_0 => "CUDA 13.0",
        }
    }

    /// 从驱动报告的 "12.6" 这样的字符串解析（取最接近的官方 wheel 版本）
    ///
    /// v3.7 逻辑：
    /// - CUDA 13.x → V13_0
    /// - CUDA 12.8+ → V12_8
    /// - CUDA 12.6+ → V12_6
    /// - CUDA 11.8+ → V11_8
    /// - 其他 → None
    pub fn from_driver_version(v: &str) -> Option<Self> {
        let major: u32 = v.split('.').next()?.parse().ok()?;
        let minor: u32 = v.split('.').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        if major >= 13 {
            Some(Self::V13_0)
        } else if major == 12 && minor >= 8 {
            Some(Self::V12_8)
        } else if major == 12 && minor >= 6 {
            Some(Self::V12_6)
        } else if major == 11 && minor >= 8 {
            Some(Self::V11_8)
        } else {
            None
        }
    }
}

/// ROCm 版本
///
/// v3.7：对齐 PyTorch 2.11 官方 wheel
/// - 删除 rocm5.7 / 6.0 / 6.1（已过时）
/// - 新增 rocm6.3 / 6.4 / 7.0 / 7.1 / 7.2
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RocmVersion {
    #[serde(rename = "rocm6.3")]
    V6_3,
    #[serde(rename = "rocm6.4")]
    V6_4,
    #[serde(rename = "rocm7.0")]
    V7_0,
    #[serde(rename = "rocm7.1")]
    V7_1,
    #[serde(rename = "rocm7.2")]
    V7_2,
}

impl RocmVersion {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::V6_3 => "rocm6.3",
            Self::V6_4 => "rocm6.4",
            Self::V7_0 => "rocm7.0",
            Self::V7_1 => "rocm7.1",
            Self::V7_2 => "rocm7.2",
        }
    }

    /// 显示名（如 "ROCm 6.4"）
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::V6_3 => "ROCm 6.3",
            Self::V6_4 => "ROCm 6.4",
            Self::V7_0 => "ROCm 7.0",
            Self::V7_1 => "ROCm 7.1",
            Self::V7_2 => "ROCm 7.2",
        }
    }
}

impl TorchVariant {
    /// 返回 (package_list, index_url) — uv pip install 参数
    pub fn install_args(&self) -> (Vec<String>, Option<String>) {
        match self {
            Self::NvidiaCuda(v) => (
                vec!["torch".into(), "torchvision".into(), "torchaudio".into()],
                Some(format!("https://download.pytorch.org/whl/{}", v.as_str())),
            ),
            Self::AmdRocm(v) => (
                vec!["torch".into(), "torchvision".into(), "torchaudio".into()],
                Some(format!("https://download.pytorch.org/whl/{}", v.as_str())),
            ),
            Self::IntelXpu => (
                vec!["torch".into(), "torchvision".into(), "torchaudio".into()],
                Some("https://download.pytorch.org/whl/xpu".to_string()),
            ),
            Self::AppleSilicon => (
                vec!["torch".into(), "torchvision".into(), "torchaudio".into()],
                // macOS 用默认 PyPI（CPU wheel）+ 内置 MPS
                None,
            ),
            Self::CpuOnly => (
                vec!["torch".into(), "torchvision".into(), "torchaudio".into()],
                Some("https://download.pytorch.org/whl/cpu".to_string()),
            ),
        }
    }

    /// 安装后验证命令（python -c "<code>"）
    pub fn verify_command(&self) -> &'static str {
        match self {
            // AMD ROCm 也通过 torch.cuda.is_available() 暴露（PyTorch 内部把 ROCm 包装成 CUDA）
            Self::NvidiaCuda(_) | Self::AmdRocm(_) => {
                "import torch; assert torch.cuda.is_available(), 'CUDA/ROCm not available'"
            }
            Self::IntelXpu => "import torch; assert torch.xpu.is_available(), 'XPU not available'",
            Self::AppleSilicon => "import torch; print('MPS available:', torch.backends.mps.is_available())",
            Self::CpuOnly => "import torch; print('torch version:', torch.__version__)",
        }
    }

    /// 平台兼容性检查（用于 UI Tab 灰显）
    pub fn is_compatible(&self, os: &str) -> bool {
        match self {
            // AMD ROCm 官方主要支持 Linux；Windows 上有实验性 HIP SDK
            Self::AmdRocm(_) => os == "linux" || os == "windows",
            // Apple Silicon 仅 macOS
            Self::AppleSilicon => os == "macos",
            _ => true,
        }
    }

    /// 显示名称（UI 用）
    ///
    /// v3.7 修复：用 CudaVersion::display_name() / RocmVersion::display_name()
    /// 替代原来的字符串 replace（原逻辑对 cu130 等会出错）
    pub fn label(&self) -> String {
        match self {
            Self::NvidiaCuda(v) => v.display_name().to_string(),
            Self::AmdRocm(v) => v.display_name().to_string(),
            Self::IntelXpu => "XPU".to_string(),
            Self::AppleSilicon => "MPS (CPU wheel)".to_string(),
            Self::CpuOnly => "CPU".to_string(),
        }
    }

    /// 兼容老 `Config.torch.cuda_version` 字段的字符串表示（v3.0 向后兼容）
    ///
    /// - NvidiaCuda → "cu118" / "cu126" / "cu128" / "cu130"
    /// - 其他 → "cpu"（老字段只能表达 NVIDIA / CPU，无法区分 AMD/Intel/Apple）
    ///
    /// 见 `commands/python_env.rs::env_change_torch_variant` 同步写入
    pub fn cuda_version_string(&self) -> String {
        match self {
            Self::NvidiaCuda(v) => v.as_str().to_string(),
            _ => "cpu".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cuda_version_str() {
        assert_eq!(CudaVersion::V11_8.as_str(), "cu118");
        assert_eq!(CudaVersion::V12_6.as_str(), "cu126");
        assert_eq!(CudaVersion::V12_8.as_str(), "cu128");
        assert_eq!(CudaVersion::V13_0.as_str(), "cu130");
    }

    #[test]
    fn test_cuda_display_name() {
        assert_eq!(CudaVersion::V11_8.display_name(), "CUDA 11.8");
        assert_eq!(CudaVersion::V12_6.display_name(), "CUDA 12.6");
        assert_eq!(CudaVersion::V12_8.display_name(), "CUDA 12.8");
        assert_eq!(CudaVersion::V13_0.display_name(), "CUDA 13.0");
    }

    #[test]
    fn test_cuda_from_driver() {
        // CUDA 13.x → V13_0
        assert_eq!(CudaVersion::from_driver_version("13.0"), Some(CudaVersion::V13_0));
        // CUDA 12.8+ → V12_8
        assert_eq!(CudaVersion::from_driver_version("12.8"), Some(CudaVersion::V12_8));
        assert_eq!(CudaVersion::from_driver_version("12.9"), Some(CudaVersion::V12_8));
        // CUDA 12.6 → V12_6
        assert_eq!(CudaVersion::from_driver_version("12.6"), Some(CudaVersion::V12_6));
        assert_eq!(CudaVersion::from_driver_version("12.7"), Some(CudaVersion::V12_6));
        // CUDA 11.8 → V11_8
        assert_eq!(CudaVersion::from_driver_version("11.8"), Some(CudaVersion::V11_8));
        // CUDA 12.4（已弃用，向下兼容到 cu118 不合理，返回 None）
        assert_eq!(CudaVersion::from_driver_version("12.4"), None);
        // 太旧
        assert_eq!(CudaVersion::from_driver_version("10.0"), None);
    }

    #[test]
    fn test_rocm_version_str() {
        assert_eq!(RocmVersion::V6_3.as_str(), "rocm6.3");
        assert_eq!(RocmVersion::V6_4.as_str(), "rocm6.4");
        assert_eq!(RocmVersion::V7_0.as_str(), "rocm7.0");
        assert_eq!(RocmVersion::V7_1.as_str(), "rocm7.1");
        assert_eq!(RocmVersion::V7_2.as_str(), "rocm7.2");
    }

    #[test]
    fn test_install_args_nvidia() {
        let (pkgs, url) = TorchVariant::NvidiaCuda(CudaVersion::V12_8).install_args();
        assert_eq!(pkgs, vec!["torch", "torchvision", "torchaudio"]);
        assert_eq!(url, Some("https://download.pytorch.org/whl/cu128".to_string()));
    }

    #[test]
    fn test_install_args_nvidia_cu130() {
        let (pkgs, url) = TorchVariant::NvidiaCuda(CudaVersion::V13_0).install_args();
        assert_eq!(pkgs.len(), 3);
        assert_eq!(url, Some("https://download.pytorch.org/whl/cu130".to_string()));
    }

    #[test]
    fn test_install_args_amd() {
        let (pkgs, url) = TorchVariant::AmdRocm(RocmVersion::V6_4).install_args();
        assert_eq!(pkgs, vec!["torch", "torchvision", "torchaudio"]);
        assert_eq!(url, Some("https://download.pytorch.org/whl/rocm6.4".to_string()));
    }

    #[test]
    fn test_install_args_intel() {
        let (pkgs, url) = TorchVariant::IntelXpu.install_args();
        assert_eq!(pkgs.len(), 3);
        assert_eq!(url, Some("https://download.pytorch.org/whl/xpu".to_string()));
    }

    #[test]
    fn test_install_args_apple() {
        let (pkgs, url) = TorchVariant::AppleSilicon.install_args();
        assert_eq!(pkgs.len(), 3);
        assert!(url.is_none(), "Apple uses default PyPI");
    }

    #[test]
    fn test_install_args_cpu() {
        let (pkgs, url) = TorchVariant::CpuOnly.install_args();
        assert_eq!(pkgs.len(), 3);
        assert_eq!(url, Some("https://download.pytorch.org/whl/cpu".to_string()));
    }

    #[test]
    fn test_is_compatible() {
        assert!(TorchVariant::NvidiaCuda(CudaVersion::V12_8).is_compatible("windows"));
        assert!(TorchVariant::NvidiaCuda(CudaVersion::V12_8).is_compatible("linux"));
        assert!(!TorchVariant::AppleSilicon.is_compatible("windows"));
        assert!(TorchVariant::AppleSilicon.is_compatible("macos"));
        assert!(TorchVariant::AmdRocm(RocmVersion::V6_4).is_compatible("linux"));
    }

    #[test]
    fn test_label_nvidia() {
        assert_eq!(TorchVariant::NvidiaCuda(CudaVersion::V13_0).label(), "CUDA 13.0");
        assert_eq!(TorchVariant::NvidiaCuda(CudaVersion::V11_8).label(), "CUDA 11.8");
    }

    #[test]
    fn test_label_rocm() {
        assert_eq!(TorchVariant::AmdRocm(RocmVersion::V7_2).label(), "ROCm 7.2");
    }

    #[test]
    fn test_serialize() {
        let v = TorchVariant::NvidiaCuda(CudaVersion::V12_8);
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("nvidia_cuda"));
        assert!(s.contains("cu128"));
    }

    #[test]
    fn test_deserialize() {
        let s = r#"{"vendor":"nvidia_cuda","version":"cu128"}"#;
        let v: TorchVariant = serde_json::from_str(s).unwrap();
        assert_eq!(v, TorchVariant::NvidiaCuda(CudaVersion::V12_8));
    }

    #[test]
    fn test_deserialize_cu130() {
        let s = r#"{"vendor":"nvidia_cuda","version":"cu130"}"#;
        let v: TorchVariant = serde_json::from_str(s).unwrap();
        assert_eq!(v, TorchVariant::NvidiaCuda(CudaVersion::V13_0));
    }
}
