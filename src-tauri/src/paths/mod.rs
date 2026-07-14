//! 顶层 paths 模块入口
//!
//! 整合路径相关的子模块：
//! - `env_paths`：**唯一**路径解析入口（v0.0.2 统一 `common/paths.rs` + 旧 `env_paths`）
//!
//! ## v0.0.2 重要变化
//!
//! - **删除** `common::paths`（v1.8 / F38 旧系统）
//! - **保留** `env_paths`（v3.x 新系统）
//! - 所有 `common::paths::xxx` 调用方迁移到 `env_paths::resolve().xxx`
//!
//! ## 用法
//!
//! ```rust
//! use crate::paths::env_paths;
//!
//! // 启动时调一次
//! let paths = env_paths::resolve()?;
//!
//! // 之后所有路径都从 paths 里取
//! let db_url = format!("sqlite://{}?mode=rwc", paths.database_path.display());
//! tokio::fs::create_dir_all(&paths.logs_dir).await?;
//! ```

pub mod env_paths;
