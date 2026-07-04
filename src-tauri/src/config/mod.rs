//! Config 模块 - TOML 配置加载/保存/无锁读
//!
//! 设计模式：
//! - Repository：持久化抽象
//! - Builder：Config::default() + update 闭包
//! - arc-swap 无锁读
//!
//! 详见 `PR/03-模块设计/01-Config.md`

pub mod atomic_write;
pub mod migrations;
pub mod models;
pub mod service;

pub use models::{
    apply_launch_patch, apply_models_patch, apply_paths_patch, apply_torch_patch,
    apply_ui_patch, Config, ConfigPatch, LaunchConfigPatch, ModelsConfigPatch,
    PathsConfigPatch, TorchConfigPatch, UiConfigPatch, LaunchMode, CudaVersion,
    ModelsMode, ModelsConfig, AdvancedArgs, PreviewMethod, LaunchConfig, PathsConfig,
    TorchConfig, UiConfig, Theme,
};
pub use service::ConfigService;

/// 共享配置类型别名
///
/// 使用 arc-swap 实现无锁读，写时原子交换整个 Config
pub type SharedConfig = arc_swap::ArcSwap<Config>;

/// 当前配置 schema 版本
///
/// 用于未来配置结构变更时的迁移
pub const CURRENT_SCHEMA_VERSION: u32 = 1;
