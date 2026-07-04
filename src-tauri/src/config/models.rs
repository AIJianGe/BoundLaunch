//! Config 数据模型
//!
//! 详见 `PR/03-模块设计/01-Config.md §4.1 核心结构体`

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 顶层 Config 结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub paths: PathsConfig,
    pub launch: LaunchConfig,
    pub torch: TorchConfig,
    pub models: ModelsConfig,
    pub ui: UiConfig,
    /// 配置 schema 版本，用于迁移
    pub schema_version: u32,
}

/// 路径配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathsConfig {
    /// ComfyUI 仓库根目录
    pub comfyui_root: PathBuf,
    /// venv 虚拟环境路径
    pub venv_path: PathBuf,
    /// Python 版本（如 "3.11"）
    pub python_version: String,
}

/// 启动配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchConfig {
    /// 运行模式
    pub mode: LaunchMode,
    /// 监听地址
    pub listen_host: String,
    /// 监听端口
    pub listen_port: u16,
    /// 自动打开浏览器
    pub auto_open_browser: bool,
    /// 预览方式
    pub preview_method: PreviewMethod,
    /// 自定义启动参数（仅 LaunchMode::Custom 时使用）
    pub custom_args: String,
    /// 高级参数
    pub advanced: AdvancedArgs,
}

/// 运行模式
///
/// 设计模式：Strategy - 不同模式构造不同命令参数
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LaunchMode {
    /// CPU 模式（--cpu --lowvram）
    Cpu,
    /// GPU 高显存（--highvram）
    GpuHigh,
    /// GPU 低显存（--lowvram）
    GpuLow,
    /// GPU 无显存（--novram）
    GpuNoVram,
    /// 自定义（使用 custom_args）
    Custom,
}

impl LaunchMode {
    /// 转为 ComfyUI 命令行参数片段
    pub fn to_args(&self) -> &'static [&'static str] {
        match self {
            Self::Cpu => &["--cpu", "--lowvram"],
            Self::GpuHigh => &["--highvram"],
            Self::GpuLow => &["--lowvram"],
            Self::GpuNoVram => &["--novram"],
            Self::Custom => &[],
        }
    }
}

/// 预览方式
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PreviewMethod {
    Latent,
    LatentUpscale,
    Autoencoder,
    None,
}

impl PreviewMethod {
    pub fn to_arg(&self) -> &'static str {
        match self {
            Self::Latent => "latent",
            Self::LatentUpscale => "latent-upscale",
            Self::Autoencoder => "autoencoder",
            Self::None => "none",
        }
    }
}

/// 高级启动参数
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdvancedArgs {
    pub use_split_cross_attention: bool,
    pub use_pytorch_cross_attention: bool,
    pub force_fp32: bool,
    pub fp16_vae: bool,
    pub bf16_vae: bool,
    pub no_half: bool,
    pub no_half_vae: bool,
    pub directml: bool,
}

impl Default for AdvancedArgs {
    fn default() -> Self {
        Self {
            use_split_cross_attention: false,
            use_pytorch_cross_attention: false,
            force_fp32: false,
            fp16_vae: false,
            bf16_vae: false,
            no_half: false,
            no_half_vae: false,
            directml: false,
        }
    }
}

/// torch 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorchConfig {
    /// CUDA 版本
    pub cuda_version: CudaVersion,
}

/// CUDA 版本
///
/// 设计模式：Strategy - 不同 CUDA 版本对应不同 torch wheel 索引
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CudaVersion {
    /// CPU 版本
    Cpu,
    /// CUDA 11.8
    Cu118,
    /// CUDA 12.1
    Cu121,
    /// CUDA 12.4
    Cu124,
}

impl CudaVersion {
    /// 转为 uv pip install 的 torch 索引 URL 后缀
    pub fn to_torch_index(&self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Cu118 => "cu118",
            Self::Cu121 => "cu121",
            Self::Cu124 => "cu124",
        }
    }

    /// 显示名
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Cu118 => "CUDA 11.8",
            Self::Cu121 => "CUDA 12.1",
            Self::Cu124 => "CUDA 12.4",
        }
    }
}

/// 模型路径配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsConfig {
    /// 模型路径模式
    pub mode: ModelsMode,
    /// 自定义根目录（ModelsMode::CustomRoot 时使用）
    pub custom_root: PathBuf,
    /// 高级配置（按类型指定，本期不用）
    pub advanced: AdvancedModels,
}

/// 模型路径模式
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelsMode {
    /// 默认（ComfyUI 根目录/models）
    Default,
    /// 自定义根目录
    CustomRoot,
    /// 高级（按类型指定）
    Advanced,
}

/// 高级模型路径配置（本期不实现）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AdvancedModels {
    // 未来按类型扩展
}

/// UI 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    /// 主题
    pub theme: Theme,
    /// 语言（本期仅 zh-CN）
    pub language: String,
    /// 启动时自动检查更新
    pub auto_check_update: bool,
    /// 关闭窗口时最小化到托盘
    pub minimize_to_tray: bool,
}

/// 主题
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    Light,
    Dark,
    Auto,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            paths: PathsConfig {
                comfyui_root: PathBuf::new(),
                venv_path: PathBuf::new(),
                python_version: "3.11".to_string(),
            },
            launch: LaunchConfig {
                mode: LaunchMode::GpuHigh,
                listen_host: "127.0.0.1".to_string(),
                listen_port: 8188,
                auto_open_browser: true,
                preview_method: PreviewMethod::Latent,
                custom_args: String::new(),
                advanced: AdvancedArgs::default(),
            },
            torch: TorchConfig {
                cuda_version: CudaVersion::Cu121,
            },
            models: ModelsConfig {
                mode: ModelsMode::Default,
                custom_root: PathBuf::new(),
                advanced: AdvancedModels::default(),
            },
            ui: UiConfig {
                theme: Theme::Auto,
                language: "zh-CN".to_string(),
                auto_check_update: true,
                minimize_to_tray: true,
            },
            schema_version: super::CURRENT_SCHEMA_VERSION,
        }
    }
}

// ============================================================================
// Patch 结构（部分更新）
//
// 设计意图：前端 `ConfigUpdate = Partial<{ ... Partial<Section> }>`，每个字段
// 都是可选的。后端收到 patch 后，把 None 的字段跳过，Some 的字段覆盖。
//
// 与「整段 deserialize」的区别：避免调用方为单个字段拼出整段 section。
// 详见 `commands/config.rs::config_update`。
// ============================================================================

/// Paths section 的 patch
#[derive(Debug, Default, Deserialize)]
pub struct PathsConfigPatch {
    pub comfyui_root: Option<PathBuf>,
    pub venv_path: Option<PathBuf>,
    pub python_version: Option<String>,
}

/// Launch section 的 patch
#[derive(Debug, Default, Deserialize)]
pub struct LaunchConfigPatch {
    pub mode: Option<LaunchMode>,
    pub listen_host: Option<String>,
    pub listen_port: Option<u16>,
    pub auto_open_browser: Option<bool>,
    pub preview_method: Option<PreviewMethod>,
    pub custom_args: Option<String>,
    pub advanced: Option<AdvancedArgs>,
}

/// Torch section 的 patch
#[derive(Debug, Default, Deserialize)]
pub struct TorchConfigPatch {
    pub cuda_version: Option<CudaVersion>,
}

/// Models section 的 patch
#[derive(Debug, Default, Deserialize)]
pub struct ModelsConfigPatch {
    pub mode: Option<ModelsMode>,
    pub custom_root: Option<PathBuf>,
    pub advanced: Option<AdvancedModels>,
}

/// UI section 的 patch
#[derive(Debug, Default, Deserialize)]
pub struct UiConfigPatch {
    pub theme: Option<Theme>,
    pub language: Option<String>,
    pub auto_check_update: Option<bool>,
    pub minimize_to_tray: Option<bool>,
}

/// 顶层 Config patch
#[derive(Debug, Default, Deserialize)]
pub struct ConfigPatch {
    pub paths: Option<PathsConfigPatch>,
    pub launch: Option<LaunchConfigPatch>,
    pub torch: Option<TorchConfigPatch>,
    pub models: Option<ModelsConfigPatch>,
    pub ui: Option<UiConfigPatch>,
}

/// 把 PathsConfigPatch 合并到 PathsConfig（Some 字段覆盖）
pub fn apply_paths_patch(cfg: &mut PathsConfig, patch: PathsConfigPatch) {
    if let Some(v) = patch.comfyui_root {
        cfg.comfyui_root = v;
    }
    if let Some(v) = patch.venv_path {
        cfg.venv_path = v;
    }
    if let Some(v) = patch.python_version {
        cfg.python_version = v;
    }
}

/// 把 LaunchConfigPatch 合并到 LaunchConfig
pub fn apply_launch_patch(cfg: &mut LaunchConfig, patch: LaunchConfigPatch) {
    if let Some(v) = patch.mode {
        cfg.mode = v;
    }
    if let Some(v) = patch.listen_host {
        cfg.listen_host = v;
    }
    if let Some(v) = patch.listen_port {
        cfg.listen_port = v;
    }
    if let Some(v) = patch.auto_open_browser {
        cfg.auto_open_browser = v;
    }
    if let Some(v) = patch.preview_method {
        cfg.preview_method = v;
    }
    if let Some(v) = patch.custom_args {
        cfg.custom_args = v;
    }
    if let Some(v) = patch.advanced {
        cfg.advanced = v;
    }
}

/// 把 TorchConfigPatch 合并到 TorchConfig
pub fn apply_torch_patch(cfg: &mut TorchConfig, patch: TorchConfigPatch) {
    if let Some(v) = patch.cuda_version {
        cfg.cuda_version = v;
    }
}

/// 把 ModelsConfigPatch 合并到 ModelsConfig
pub fn apply_models_patch(cfg: &mut ModelsConfig, patch: ModelsConfigPatch) {
    if let Some(v) = patch.mode {
        cfg.mode = v;
    }
    if let Some(v) = patch.custom_root {
        cfg.custom_root = v;
    }
    if let Some(v) = patch.advanced {
        cfg.advanced = v;
    }
}

/// 把 UiConfigPatch 合并到 UiConfig
pub fn apply_ui_patch(cfg: &mut UiConfig, patch: UiConfigPatch) {
    if let Some(v) = patch.theme {
        cfg.theme = v;
    }
    if let Some(v) = patch.language {
        cfg.language = v;
    }
    if let Some(v) = patch.auto_check_update {
        cfg.auto_check_update = v;
    }
    if let Some(v) = patch.minimize_to_tray {
        cfg.minimize_to_tray = v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let cfg = Config::default();
        assert_eq!(cfg.launch.listen_port, 8188);
        assert_eq!(cfg.torch.cuda_version, CudaVersion::Cu121);
        assert_eq!(cfg.ui.language, "zh-CN");
        assert_eq!(cfg.schema_version, 1);
    }

    #[test]
    fn test_launch_mode_to_args() {
        assert_eq!(LaunchMode::Cpu.to_args(), &["--cpu", "--lowvram"]);
        assert_eq!(LaunchMode::GpuHigh.to_args(), &["--highvram"]);
        assert!(LaunchMode::Custom.to_args().is_empty());
    }

    #[test]
    fn test_cuda_version_display() {
        assert_eq!(CudaVersion::Cu121.display_name(), "CUDA 12.1");
        assert_eq!(CudaVersion::Cpu.to_torch_index(), "cpu");
    }

    #[test]
    fn test_serde_roundtrip() {
        let cfg = Config::default();
        let toml_str = toml::to_string(&cfg).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.launch.listen_port, cfg.launch.listen_port);
        assert_eq!(parsed.torch.cuda_version, cfg.torch.cuda_version);
    }
}
