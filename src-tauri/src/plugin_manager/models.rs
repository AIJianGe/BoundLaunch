//! PluginManager 数据模型
//!
//! 详见 `PR/03-模块设计/04-PluginManager.md §3 接口签名`

use std::path::PathBuf;
use serde::Serialize;

/// 单个插件信息
#[derive(Debug, Clone, Serialize)]
pub struct PluginInfo {
    pub name: String,
    /// 目录名（可能是 `xxx.disabled`）
    pub dir_name: String,
    pub enabled: bool,
    pub git_url: Option<String>,
    pub current_commit: String,
    pub current_branch: Option<String>,
    /// v3.x：当前检出的 ref（tag 名或 commit short）
    /// 用于 UI 显示"在 v1.2.0" vs "在 main"
    pub current_ref: Option<String>,
    /// v3.x：上次切版本前的 commit（用于回滚）
    pub backup_commit: Option<String>,
    /// v3.x：是否处于 detached HEAD 状态（切到 tag 后会有这个状态）
    pub is_detached: bool,
    /// `None` = 未检查；`Some(bool)` = 已检查
    pub has_updates: Option<bool>,
    pub has_local_changes: bool,
    pub installed_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 从 `pyproject.toml` / `__init__.py` 读（延迟加载）
    pub description: Option<String>,
    pub requirements_installed: bool,
}

/// 插件列表查询结果
#[derive(Debug, Clone, Serialize)]
pub struct PluginListResult {
    pub plugins: Vec<PluginInfo>,
    pub fetched_at: chrono::DateTime<chrono::Utc>,
}

/// 插件更新结果
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpdateResult {
    Updated { from: String, to: String },
    AlreadyUpToDate,
}

/// 插件卸载结果
#[derive(Debug, Clone, Serialize)]
pub struct UninstallResult {
    /// 回收站路径
    pub moved_to: PathBuf,
    pub recoverable: bool,
}

/// 插件更新检查结果
#[derive(Debug, Clone, Serialize)]
pub struct PluginUpdateInfo {
    pub name: String,
    pub has_update: bool,
    pub current_commit: String,
    pub latest_commit: Option<String>,
}

/// 远程仓库 tag 信息（用于安装时选择版本）
#[derive(Debug, Clone, Serialize)]
pub struct RemoteTagInfo {
    /// tag 名称，如 "v1.2.0"
    pub name: String,
    /// tag 对应的 commit hash
    pub commit: String,
}

/// 本地仓库的 ref 信息（tag + branch，用于切版本时选择目标）
#[derive(Debug, Clone, Serialize)]
pub struct LocalRefInfo {
    /// "tag" | "branch"
    pub kind: String,
    /// 名称，如 "v1.2.0" 或 "main"
    pub name: String,
    /// 对应的 commit hash
    pub commit: String,
    /// 是否是当前 HEAD
    pub is_current: bool,
}

/// 切换版本结果（含回滚信息）
#[derive(Debug, Clone, Serialize)]
pub struct SwitchResult {
    /// 切换后的 plugin info
    pub plugin: PluginInfo,
    /// 切换前的 commit（供前端"回滚"按钮使用）
    pub previous_commit: String,
    /// 切换后是否需要重启 ComfyUI（如果 ComfyUI 在跑）
    pub need_restart: bool,
}

/// 插件操作进度（用于 install / update / install_requirements 流式推送）
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum PluginProgress {
    /// 正在克隆（0-100）
    Cloning { percent: u32 },
    /// 正在装依赖（0-100）
    InstallingRequirements { percent: u32 },
    /// v3.x：依赖装过程中的中间 percent（带 plugin 名，用于多插件并发场景）
    RequirementsPercent { plugin: String, percent: u32 },
    /// 正在拉取更新
    Pulling { percent: u32 },
    /// v3.x：正在切版本（fetch + checkout，0-100）
    Switching { percent: u32 },
    /// 完成
    Done,
    /// 失败
    Failed { error: String },
}

/// PluginManager 错误类型
///
/// 详见 `PR/03-模块设计/04-PluginManager.md §4.2`
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("插件不存在: {0}")]
    NotFound(String),
    #[error("插件已存在: {0}")]
    AlreadyExists(String),
    #[error("git URL 无效: {0}")]
    InvalidUrl(String),
    #[error("git clone 失败: {stderr}")]
    CloneFailed { stderr: String },
    #[error("git pull 失败: {stderr}")]
    PullFailed { stderr: String },
    #[error("插件目录不可写: {0}")]
    NotWritable(PathBuf),
    #[error("插件正在被其他操作占用: {0}")]
    PluginBusy(String),
    #[error("插件 requirements 安装失败: {detail}")]
    RequirementsFailed { detail: String },
    #[error("回收站目录创建失败: {0}")]
    TrashCreateFailed(PathBuf),
    #[error("venv 未初始化")]
    VenvNotReady,
    #[error("git 操作超时（{timeout}s）")]
    Timeout { timeout: u64 },
    #[error("git 操作失败: {0}")]
    GitError(#[from] git2::Error),
    #[error("IO 错误: {0}")]
    IoError(#[from] std::io::Error),
}

impl From<PluginError> for String {
    fn from(e: PluginError) -> Self {
        e.to_string()
    }
}
