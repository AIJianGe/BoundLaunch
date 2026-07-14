//! 公共工具模块
//!
//! **v0.0.2.1 变化**：删除 `paths` 子模块（合并到 `crate::paths::env_paths`）

pub mod ansi;
pub mod line_collector;
pub mod platform;
pub mod process_util;
pub mod subprocess;
