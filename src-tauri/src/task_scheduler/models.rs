//! TaskScheduler 数据模型
//!
//! 详见 `PR/03-模块设计/08-TaskScheduler.md §3 接口签名`

use serde::{Deserialize, Serialize};

/// 任务唯一标识（UUID v4 字符串）
pub type TaskId = String;

/// 任务类型枚举：决定默认优先级与历史归档分组
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// 克隆 ComfyUI 仓库
    CloneRepo,
    /// 拉取远程 tag 列表
    FetchTags,
    /// 切换到指定版本
    Checkout,
    /// 安装/切换 torch
    InstallTorch,
    /// 安装 requirements.txt
    InstallRequirements,
    /// 安装插件
    PluginInstall,
    /// 更新插件
    PluginUpdate,
    /// 扫描模型子目录
    ScanModels,
    /// 调用方自定义
    Custom,
    /// F32 新增：创建 venv
    CreateVenv,
    /// F32 新增：切换 torch 变体（NVIDIA/AMD/Intel/Apple/CPU）
    SwitchTorchVariant,
    /// F32 新增：重建 venv（5 步事务）
    RebuildVenv,
    /// F32 新增：切换 Python 版本（5 步事务 + 备份回滚）
    SwitchPython,
    /// v1.8 / F36：环境修复（诊断 + 自动修复），见 `python_env::recovery`
    EnvRepair,
}

impl TaskKind {
    /// 默认优先级
    ///
    /// - 版本切换与 torch 安装为 High（用户主动等待）
    /// - F32 新增 4 个环境操作为 High（用户主动等待）
    /// - 插件安装/更新、requirements 为 Normal
    /// - 扫描类为 Low
    pub fn default_priority(&self) -> TaskPriority {
        match self {
            TaskKind::Checkout | TaskKind::InstallTorch => TaskPriority::High,
            // F32 新增：4 个环境长任务均为 High（用户主动触发并等待）
            TaskKind::CreateVenv
            | TaskKind::SwitchTorchVariant
            | TaskKind::RebuildVenv
            | TaskKind::SwitchPython
            | TaskKind::EnvRepair => TaskPriority::High,
            TaskKind::CloneRepo
            | TaskKind::FetchTags
            | TaskKind::InstallRequirements
            | TaskKind::PluginInstall
            | TaskKind::PluginUpdate
            | TaskKind::Custom => TaskPriority::Normal,
            TaskKind::ScanModels => TaskPriority::Low,
        }
    }

    /// 序列化为字符串（用于 LogStore `kind` 字段）
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskKind::CloneRepo => "clone_repo",
            TaskKind::FetchTags => "fetch_tags",
            TaskKind::Checkout => "checkout",
            TaskKind::InstallTorch => "install_torch",
            TaskKind::InstallRequirements => "install_requirements",
            TaskKind::PluginInstall => "plugin_install",
            TaskKind::PluginUpdate => "plugin_update",
            TaskKind::ScanModels => "scan_models",
            TaskKind::Custom => "custom",
            // F32 新增
            TaskKind::CreateVenv => "create_venv",
            TaskKind::SwitchTorchVariant => "switch_torch_variant",
            TaskKind::RebuildVenv => "rebuild_venv",
            TaskKind::SwitchPython => "switch_python",
            TaskKind::EnvRepair => "env_repair",
        }
    }
}

impl std::fmt::Display for TaskKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 任务优先级
///
/// 数值越小优先级越高（Ord 顺序：High < Normal < Low）
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    /// 0：版本切换 / torch 安装（用户主动等待）
    High,
    /// 1：插件安装更新 / requirements
    Normal,
    /// 2：扫描 / 预热类任务
    Low,
}

impl TaskPriority {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskPriority::High => "high",
            TaskPriority::Normal => "normal",
            TaskPriority::Low => "low",
        }
    }
}

/// 任务执行结果（业务自定义载荷，JSON 序列化后存历史）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    /// 一行摘要
    pub summary: String,
    /// 任意 JSON 载荷
    pub payload: Option<serde_json::Value>,
}

impl TaskResult {
    pub fn new(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            payload: None,
        }
    }

    pub fn with_payload(mut self, payload: serde_json::Value) -> Self {
        self.payload = Some(payload);
        self
    }
}

/// 任务状态
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum TaskStatus {
    /// 排队等待 permit
    Queued,
    /// 运行中，progress 0..=100
    Running { progress: u8 },
    /// 成功完成
    Completed,
    /// 失败
    Failed { error: String },
    /// 被取消
    Cancelled,
}

impl TaskStatus {
    /// 是否为终态（Completed / Failed / Cancelled）
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TaskStatus::Completed | TaskStatus::Failed { .. } | TaskStatus::Cancelled
        )
    }

    /// 终态的字符串标识（用于 LogStore `status` 字段）
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Queued => "queued",
            TaskStatus::Running { .. } => "running",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed { .. } => "failed",
            TaskStatus::Cancelled => "cancelled",
        }
    }
}

/// 任务信息（对外暴露的快照）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskInfo {
    pub id: TaskId,
    pub kind: TaskKind,
    pub name: String,
    pub priority: TaskPriority,
    pub status: TaskStatus,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// TaskScheduler 错误类型
///
/// 详见 `PR/03-模块设计/08-TaskScheduler.md §4.2`
#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error("任务队列已满（最大 {max} 个排队）")]
    QueueFull { max: usize },
    #[error("任务不存在: {0}")]
    NotFound(TaskId),
    #[error("任务已终态，无法取消/等待: {0}")]
    AlreadyCompleted(TaskId),
    #[error("取消失败: {0}")]
    CancelFailed(String),
    #[error("任务执行失败: {error}")]
    ActionFailed { error: String },
    #[error("任务在等待期间被取消")]
    WaitCancelled,
    #[error("LogStore 错误: {0}")]
    LogStore(String),
}

impl From<crate::log_store::LogStoreError> for TaskError {
    fn from(e: crate::log_store::LogStoreError) -> Self {
        Self::LogStore(e.to_string())
    }
}

impl From<TaskError> for String {
    fn from(e: TaskError) -> Self {
        e.to_string()
    }
}
