//! 自动更新模块
//!
//! ## 流程
//!
//! ```text
//! 1. AboutPage: 用户点"检查更新"
//!    └→ check_update()
//!       └→ GET https://api.github.com/repos/AIJianGe/BoundLaunch/releases/latest
//!          → UpdateInfo { has_update, latest_version, download_url, sha256, ... }
//!
//! 2. 用户点"立即更新"
//!    └→ download_update()
//!       └→ reqwest 拉 zip → .boundlaunch/update-staging/<version>/
//!          + emit `update_progress` 事件
//!          + SHA256 校验（可选）
//!
//! 3. download_update 完成后自动调 apply_update()
//!    └→ staging 白名单拷贝到 .boundlaunch/update-pending/
//!       + emit `update_ready` 事件
//!       + 弹窗提示用户重启
//!
//! 4. 用户重启
//!    └→ 启动期 apply_pending_update()
//!       + rename .new → 标准名
//!       + resources/uv/ merge 到 env_root
//!       + 清空 pending
//!       → 新版本生效
//! ```
//!
//! ## 数据保护（关键）
//!
//! - `launcher-portable.dat`、`.boundlaunch/launcher.db`、`ComfyUI/`、`data/venv/`、
//!   模型、插件等所有"非白名单"文件**完全不动**
//! - 即使用户把整个目录复制到新位置，更新也不会破坏数据

pub mod apply;
pub mod download;
pub mod manifest;
pub mod paths;

// 重新导出
pub use apply::{apply_pending_update, apply_update, ApplyPendingResult, ApplyResult};
pub use download::{download_and_extract, UpdateProgress, EVENT_UPDATE_PROGRESS};
pub use manifest::{
    build_update_info, find_portable_zip, find_sha256_asset, is_newer, strip_v_prefix, GithubAsset,
    GithubRelease, ManifestClient, ManifestError, UpdateInfo,
};
