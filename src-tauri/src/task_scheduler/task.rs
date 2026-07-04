//! 任务定义与内部句柄
//!
//! 详见 `PR/03-模块设计/08-TaskScheduler.md §3 接口签名`

use std::future::Future;

use futures::future::BoxFuture;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::models::{TaskId, TaskInfo, TaskKind, TaskPriority, TaskResult, TaskStatus};
use super::progress::ProgressSender;

/// 任务定义：由调用方构造后提交给 TaskScheduler
///
/// `action` 闭包接收：
/// - `CancellationToken`：用于检查取消信号
/// - `ProgressSender`：用于上报进度与消息
///
/// `action` 应在每个"可中断点"周期性检查 `cancel_token.is_cancelled()`，
/// 长循环建议每 N 次迭代或每 100ms 检查一次。
pub struct TaskDef {
    pub kind: TaskKind,
    /// 人类可读名称，如 "克隆 ComfyUI 仓库"
    pub name: String,
    /// None 时取 kind 的默认优先级
    pub priority: Option<TaskPriority>,
    /// action 闭包：接收取消令牌与进度发送器，返回业务结果
    pub action: Box<
        dyn FnOnce(CancellationToken, ProgressSender) -> BoxFuture<'static, Result<TaskResult, String>>
            + Send,
    >,
}

impl std::fmt::Debug for TaskDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskDef")
            .field("kind", &self.kind)
            .field("name", &self.name)
            .field("priority", &self.priority)
            .field("action", &"<closure>")
            .finish()
    }
}

/// 内部任务句柄（不对外暴露）
pub(crate) struct TaskHandle {
    pub info: TaskInfo,
    /// 取消令牌（Queued / Running 时存在；终态后置 None）
    pub cancel_token: Option<CancellationToken>,
    /// spawn 的 join handle（用于 wait）
    pub join: Option<JoinHandle<FinalOutcome>>,
    /// action 的最终结果（终态后填充，wait 直接返回缓存）
    pub final_result: Option<Result<TaskResult, FinalErr>>,
}

/// 调度 task 完成后通过 JoinHandle 传递给主表的最终产物
pub(crate) type FinalOutcome = std::result::Result<TaskResult, FinalErr>;

/// 区分"业务 Err"与"取消"与"panic"
#[derive(Debug)]
pub(crate) enum FinalErr {
    /// action 返回 Err（业务失败）
    ActionFailed(String),
    /// action 内部检测到取消信号后返回（无论返回什么都被视为取消）
    Cancelled,
    /// action panic（catch_unwind 捕获）
    Panicked(String),
}

impl TaskHandle {
    /// 创建新任务句柄（初始状态 Queued）
    pub fn new(id: TaskId, kind: TaskKind, name: String, priority: TaskPriority) -> (Self, CancellationToken) {
        let token = CancellationToken::new();
        let info = TaskInfo {
            id: id.clone(),
            kind,
            name,
            priority,
            status: TaskStatus::Queued,
            started_at: None,
            completed_at: None,
        };
        let handle = Self {
            info,
            cancel_token: Some(token.clone()),
            join: None,
            final_result: None,
        };
        (handle, token)
    }
}

/// trait alias：标记可被 catch_unwind 包裹的 Future
///
/// 实际上 BoxFuture 已经满足 Send，这里仅文档化要求
pub trait ActionFuture: Future<Output = Result<TaskResult, String>> + Send {}
impl<F> ActionFuture for F where F: Future<Output = Result<TaskResult, String>> + Send {}
