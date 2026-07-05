//! torch 安装变体抽象（v3.0 新增）
//!
//! 支持 5 厂商：NVIDIA CUDA / AMD ROCm / Intel XPU / Apple Silicon MPS / CPU
//!
//! 每个变体统一通过 `install_args` 返回 uv pip install 参数，
//! 通过 `verify_command` 返回安装后验证命令。
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum CudaVersion {
    #[serde(rename = "cu118")]
    V11_8,
    #[serde(rename = "cu121")]
    V12_1,
    #[serde(rename = "cu124")]
    V12_4,
}

impl CudaVersion {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::V11_8 => "cu118",
            Self::V12_1 => "cu121",
            Self::V12_4 => "cu124",
        }
    }

    /// 从驱动报告的 "12.6" 这样的字符串解析（取主版本兼容）
    pub fn from_driver_version(v: &str) -> Option<Self> {
        let major: u32 = v.split('.').next()?.parse().ok()?;
        let minor: u32 = v.split('.').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        // CUDA 12.4+ 需要驱动 >= 550；12.1+ 需要 >= 530；11.8+ 需要 >= 450
        if major >= 12 && minor >= 4 {
            Some(Self::V12_4)
        } else if major >= 12 {
            Some(Self::V12_1)
        } else if major >= 11 {
            Some(Self::V11_8)
        } else {
            None
        }
    }
}

/// ROCm 版本
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RocmVersion {
    #[serde(rename = "rocm5.7")]
    V5_7,
    #[serde(rename = "rocm6.0")]
    V6_0,
    #[serde(rename = "rocm6.1")]
    V6_1,
}

impl RocmVersion {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::V5_7 => "rocm5.7",
            Self::V6_0 => "rocm6.0",
            Self::V6_1 => "rocm6.1",
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
    pub fn label(&self) -> String {
        match self {
            Self::NvidiaCuda(v) => format!("CUDA {}", v.as_str().trim_start_matches("cu").replace("1", " ").trim()),
            Self::AmdRocm(v) => format!("ROCm {}", v.as_str().trim_start_matches("rocm")),
            Self::IntelXpu => "XPU".to_string(),
            Self::AppleSilicon => "MPS (CPU wheel)".to_string(),
            Self::CpuOnly => "CPU".to_string(),
        }
    }

    /// 兼容老 `Config.torch.cuda_version` 字段的字符串表示（v3.0 向后兼容）
    ///
    /// - NvidiaCuda → "cu118" / "cu121" / "cu124"
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
        assert_eq!(CudaVersion::V12_1.as_str(), "cu121");
        assert_eq!(CudaVersion::V12_4.as_str(), "cu124");
    }

    #[test]
    fn test_cuda_from_driver() {
        assert_eq!(CudaVersion::from_driver_version("12.6"), Some(CudaVersion::V12_4));
        assert_eq!(CudaVersion::from_driver_version("12.4"), Some(CudaVersion::V12_4));
        assert_eq!(CudaVersion::from_driver_version("12.1"), Some(CudaVersion::V12_1));
        assert_eq!(CudaVersion::from_driver_version("11.8"), Some(CudaVersion::V11_8));
        assert_eq!(CudaVersion::from_driver_version("10.0"), None);
    }

    #[test]
    fn test_install_args_nvidia() {
        let (pkgs, url) = TorchVariant::NvidiaCuda(CudaVersion::V12_1).install_args();
        assert_eq!(pkgs, vec!["torch", "torchvision", "torchaudio"]);
        assert_eq!(url, Some("https://download.pytorch.org/whl/cu121".to_string()));
    }

    #[test]
    fn test_install_args_amd() {
        let (pkgs, url) = TorchVariant::AmdRocm(RocmVersion::V6_0).install_args();
        assert_eq!(pkgs, vec!["torch", "torchvision", "torchaudio"]);
        assert_eq!(url, Some("https://download.pytorch.org/whl/rocm6.0".to_string()));
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
        assert!(TorchVariant::NvidiaCuda(CudaVersion::V12_1).is_compatible("windows"));
        assert!(TorchVariant::NvidiaCuda(CudaVersion::V12_1).is_compatible("linux"));
        assert!(!TorchVariant::AppleSilicon.is_compatible("windows"));
        assert!(TorchVariant::AppleSilicon.is_compatible("macos"));
        assert!(TorchVariant::AmdRocm(RocmVersion::V6_0).is_compatible("linux"));
    }

    #[test]
    fn test_serialize() {
        let v = TorchVariant::NvidiaCuda(CudaVersion::V12_1);
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("nvidia_cuda"));
        assert!(s.contains("cu121"));
    }

    #[test]
    fn test_deserialize() {
        let s = r#"{"vendor":"nvidia_cuda","version":"cu121"}"#;
        let v: TorchVariant = serde_json::from_str(s).unwrap();
        assert_eq!(v, TorchVariant::NvidiaCuda(CudaVersion::V12_1));
    }
}
