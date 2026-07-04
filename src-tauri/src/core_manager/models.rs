//! CoreManager 值对象
//!
//! 详见 `PR/03-模块设计/03-CoreManager.md §3 接口签名`

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 单个 Git tag 信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagInfo {
    pub name: String,
    /// 是否为稳定版（严格 vX.Y.Z 格式，无 rc/beta/pre/dev 后缀）
    pub is_stable: bool,
    pub commit: String,
    pub date: DateTime<Utc>,
}

/// ComfyUI 仓库当前状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreStatus {
    /// 当前 HEAD 对应的 tag 名（无 tag 时为 None）
    pub current_version: Option<String>,
    pub current_commit: String,
    /// 工作区是否有未提交改动
    pub has_local_changes: bool,
    /// 最新稳定版 tag（list_tags 后填充）
    pub latest_stable: Option<String>,
    /// 仓库是否已克隆
    pub is_clone_done: bool,
}

/// checkout 操作结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum CheckoutResult {
    /// 已切换到新 tag
    Switched {
        from: Option<String>,
        to: String,
    },
    /// 已在目标 tag，无操作
    AlreadyOnTag(String),
    /// 因有本地改动，先 stash 再切换
    StashedAndSwitched {
        stash_ref: String,
        from: String,
        to: String,
    },
}

/// clone 进度
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloneProgress {
    /// 已接收字节数
    pub received_bytes: usize,
    /// 已接收对象数
    pub received_objects: usize,
    /// 总对象数（估算）
    pub total_objects: usize,
    /// 进度百分比（0-100）
    pub percent: u8,
}

/// clone 进度回调 trait（便于 mock）
pub trait ProgressCallback: Send + 'static {
    fn on_progress(&self, progress: CloneProgress);
}

/// ComfyUI 仓库 URL（默认）
pub const COMFYUI_REPO_URL: &str = "https://github.com/comfyanonymous/ComfyUI.git";

/// 自动 stash 命名前缀
pub const AUTO_STASH_PREFIX: &str = "launcher-auto-stash";
