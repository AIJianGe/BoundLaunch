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

/// tag 分类（v3.1 / F26 决策 9：SemVer 规则 + 决策 7：NTab 双分类）
///
/// - stable：严格 `vX.Y.Z` 格式（无后缀）
/// - prerelease：`vX.Y.Z-rc1` / `vX.Y.Z-beta` / `vX.Y.Z-pre` 等带后缀
///
/// 非 SemVer 格式的 tag（如 `latest` / `master`）会被过滤掉。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassifiedTags {
    /// 稳定版 tag（按版本倒序）
    pub stable: Vec<TagInfo>,
    /// 预发布版 tag（按版本倒序）
    pub prerelease: Vec<TagInfo>,
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

/// 切换版本前置检查结果（v3.1 / F26 决策 5）
///
/// 在调用 `switch_version` 前由前端调用 `check_switch_prerequisites` 获取。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitchPrerequisites {
    /// 是否允许切换
    pub can_switch: bool,
    /// ComfyUI 是否正在运行（运行中拒绝切换）
    pub comfyui_running: bool,
    /// 工作区是否有未提交改动（脏状态拒绝切换）
    pub has_local_changes: bool,
    /// 当前 tag（用于回滚）
    pub current_tag: Option<String>,
    /// 阻止原因（can_switch = false 时填充）
    pub block_reason: Option<String>,
}

/// 切换版本结果（v3.1 / F26 决策 6：全部回滚）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SwitchVersionResult {
    /// 切换成功
    Success {
        from: Option<String>,
        to: String,
        /// venv 是否被重建（决策 3：总是重建）
        venv_rebuilt: bool,
        /// models 链接是否重建
        models_link_rebuilt: bool,
        /// requirements 是否已重新安装
        requirements_reinstalled: bool,
    },
    /// 切换失败但已回滚到原版本
   RolledBack {
        to: String,
        error: String,
        /// 回滚是否完整（git checkout 已恢复；venv 可能已损坏）
        rollback_clean: bool,
    },
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
///
/// v3.1 更新：从旧仓库 `comfyanonymous/ComfyUI`（组织已迁移，已停止更新）切到
/// 官方新仓库 `Comfy-Org/ComfyUI`。该常量是所有 tag 拉取/clone 的唯一来源。
pub const COMFYUI_REPO_URL: &str = "https://github.com/Comfy-Org/ComfyUI.git";

/// 自动 stash 命名前缀
pub const AUTO_STASH_PREFIX: &str = "launcher-auto-stash";

// ============================================================================
// F31：仓库地址切换与备份恢复
// ============================================================================

/// 备份元信息（每个备份目录下的 .backup_meta.json）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupMeta {
    /// 备份时间
    pub backed_up_at: chrono::DateTime<chrono::Utc>,
    /// 备份时的仓库 URL（含 token，用于恢复时写回 Config）
    pub repo_url: String,
    /// 脱敏 URL（用于 UI 显示）
    pub repo_url_masked: String,
    /// 备份时的 tag（可能为 None）
    pub current_tag: Option<String>,
    /// 备份时的 commit SHA
    pub current_commit: String,
    /// 备份时的 comfyui_root 路径
    pub comfyui_root_at_backup: String,
    /// 备份时 custom_nodes 数量
    pub custom_nodes_count: usize,
}

/// 备份信息（前端展示用，对应 `core_list_backups` 返回值）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupInfo {
    /// 备份目录名（如 "ComfyUI_bak01"）
    pub name: String,
    /// 完整路径
    pub path: String,
    /// 备份时间（ISO8601）
    pub backed_up_at: String,
    /// 脱敏仓库 URL
    pub repo_url_masked: String,
    /// 备份时的 tag
    pub current_tag: Option<String>,
    /// 备份时的 commit SHA
    pub current_commit: String,
    /// 备份目录大小（字节）
    pub size_bytes: u64,
}

/// 切换仓库地址结果（discriminated union）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SwitchRepoResult {
    /// 切换成功
    Success {
        from_url: String,
        to_url: String,
        /// 备份目录名（None = 无需备份，如首次克隆）
        backup_name: Option<String>,
        /// 克隆耗时（毫秒）
        clone_elapsed_ms: u64,
    },
    /// 切换失败但已回滚
    RolledBack {
        to_url: String,
        error: String,
        /// 回滚是否完整
        rollback_clean: bool,
    },
}
