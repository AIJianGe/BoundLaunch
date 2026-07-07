//! TaskScheduler 模块
//!
//! 详见 `PR/03-模块设计/08-TaskScheduler.md`
//!
//! ## 职责
//! - 统一管理所有长任务（>1s）的提交、调度、进度推送、取消
//! - 替代各模块散落的进度 event，统一为 `task_progress` event
//! - 任务并发上限控制（默认 3，可经构造函数覆盖）
//! - 任务优先级队列（High / Normal / Low）
//! - 任务历史记录（通过 LogStore 持久化到 SQLite）
//! - 取消机制（CancellationToken 在 action 内部传播）
//!
//! ## 设计模式
//! - **Command**：Task 抽象（TaskDef + action 闭包）
//! - **Observer**：progress event 推送给前端
//! - **Semaphore**：并发上限控制
//! - **Cache-Aside**：tasks 表内存缓存 + LRU 淘汰

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use parking_lot::RwLock as PlRwLock;
use tauri::AppHandle;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::log_store::LogStoreService;

use self::progress::{emit_queued, emit_terminal};
use self::runner::run_action;
use self::task::{FinalErr, FinalOutcome, TaskHandle};

pub mod factory;
pub mod history;
pub mod models;
pub mod progress;
pub mod runner;
pub mod task;

pub use models::{
    TaskError, TaskId, TaskInfo, TaskKind, TaskPriority, TaskResult, TaskStatus,
};

/// 重新导出 TaskDef 以便外部模块（如 core_manager::switcher）构造任务
pub use task::TaskDef;

/// 内存中保留的终态任务上限（超过则淘汰最早的）
const MAX_TERMINAL_KEEP: usize = 200;

/// wait 循环查询间隔
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// 共享内部状态（spawn 闭包捕获用 Arc）
struct Inner {
    /// 所有任务表（含已完成，直到被淘汰）
    tasks: PlRwLock<HashMap<TaskId, TaskHandle>>,
    /// 取消令牌表（独立 DashMap，便于 cancel 路径无锁查找）
    cancel_tokens: DashMap<TaskId, CancellationToken>,
    /// 当前排队数
    queued_count: PlRwLock<usize>,
}

/// TaskScheduler 服务主体
///
/// 设计模式：
/// - **单例**：通过 AppState 全局共享
/// - **Semaphore**：max_concurrent 并发上限
/// - **DashMap**：cancel_tokens 无锁并发读
pub struct TaskSchedulerService {
    inner: Arc<Inner>,
    /// 并发上限信号量
    semaphore: Arc<Semaphore>,
    /// 排队上限
    max_queued: usize,
    /// Tauri 句柄（emit 进度 event；测试中可为 None）
    app: Option<AppHandle>,
    /// LogStore 句柄（写任务历史）
    log_store: Arc<LogStoreService>,
}

impl TaskSchedulerService {
    /// 构造（生产环境使用，接收 AppHandle 用于 emit 事件）
    ///
    /// - `max_concurrent`：并发上限（默认建议 3）
    /// - `max_queued`：排队上限（默认建议 20）
    pub fn new(
        max_concurrent: usize,
        max_queued: usize,
        app: AppHandle,
        log_store: Arc<LogStoreService>,
    ) -> Self {
        Self::new_impl(max_concurrent, max_queued, Some(app), log_store)
    }

    /// 构造（测试环境使用，无 AppHandle，emit 跳过）
    pub fn new_for_test(
        max_concurrent: usize,
        max_queued: usize,
        log_store: Arc<LogStoreService>,
    ) -> Self {
        Self::new_impl(max_concurrent, max_queued, None, log_store)
    }

    fn new_impl(
        max_concurrent: usize,
        max_queued: usize,
        app: Option<AppHandle>,
        log_store: Arc<LogStoreService>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                tasks: PlRwLock::new(HashMap::new()),
                cancel_tokens: DashMap::new(),
                queued_count: PlRwLock::new(0),
            }),
            semaphore: Arc::new(Semaphore::new(max_concurrent.max(1))),
            max_queued: max_queued.max(1),
            app,
            log_store,
        }
    }

    /// 提交任务：入队后立即返回 task_id，不等执行
    ///
    /// 流程：
    /// 1. 生成 task_id（UUID v4）
    /// 2. 检查排队上限
    /// 3. 创建 CancellationToken，写入 cancel_tokens
    /// 4. 任务句柄入 tasks 表（status = Queued）
    /// 5. emit("task_queued")
    /// 6. spawn 调度 task：acquire permit → run_action → 更新状态 → emit terminal → LogStore
    pub async fn submit(&self, def: TaskDef) -> Result<TaskId, TaskError> {
        let task_id = uuid::Uuid::new_v4().to_string();
        let priority = def.priority.unwrap_or_else(|| def.kind.default_priority());
        let kind = def.kind;
        let name = def.name.clone();
        // ✅ P2-1：从 TaskDef 读取 parent_id（submit_child 自动注入）
        let parent_id = def.parent_id.clone();

        // 2. 检查排队上限
        {
            let mut q = self.inner.queued_count.write();
            if *q >= self.max_queued {
                tracing::warn!(max_queued = self.max_queued, "submit rejected, queue full");
                return Err(TaskError::QueueFull { max: self.max_queued });
            }
            *q += 1;
        }

        // 3. 创建句柄 + token（✅ P2-1 传入 parent_id）
        let (handle, cancel_token) = TaskHandle::new(
            task_id.clone(),
            kind,
            name.clone(),
            priority,
            parent_id.clone(),
        );
        self.inner.cancel_tokens.insert(task_id.clone(), cancel_token.clone());

        // 4. 入 tasks 表
        {
            let mut tasks = self.inner.tasks.write();
            tasks.insert(task_id.clone(), handle);
        }

        // 5. emit 入队事件
        emit_queued(
            &self.app,
            &task_id,
            kind.as_str(),
            &name,
            priority.as_str(),
        );
        tracing::info!(?task_id, ?kind, ?priority, name = %name, ?parent_id, "task submitted");

        // 6. spawn 调度 task
        let app = self.app.clone();
        let semaphore = self.semaphore.clone();
        let log_store = self.log_store.clone();
        let inner = self.inner.clone();
        let task_id_for_spawn = task_id.clone();
        let parent_id_for_spawn = parent_id.clone();

        tokio::spawn(async move {
            // 6.1 acquire permit
            let permit = match semaphore.acquire_owned().await {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(?task_id_for_spawn, error = ?e, "semaphore closed");
                    Inner::mark_terminal(
                        &inner,
                        &app,
                        &log_store,
                        &task_id_for_spawn,
                        TaskStatus::Failed {
                            error: "semaphore closed".to_string(),
                        },
                        Some("任务调度器信号量已关闭".to_string()),
                    ).await;
                    return;
                }
            };
            tracing::info!(?task_id_for_spawn, "acquired permit, task running");

            // 6.2 状态转为 Running{progress:0}
            {
                let mut tasks = inner.tasks.write();
                if let Some(h) = tasks.get_mut(&task_id_for_spawn) {
                    h.info.status = TaskStatus::Running { progress: 0 };
                    h.info.started_at = Some(chrono::Utc::now());
                }
            }
            // 排队计数减一
            {
                let mut q = inner.queued_count.write();
                *q = q.saturating_sub(1);
            }

            // 6.3-6.5 执行 action（catch_unwind + cancel 传播）
            // ✅ P2-1：把 parent_id 传给 run_action，让子任务日志 parent_task_id 正确
            let outcome: FinalOutcome = run_action(
                app.clone(),
                task_id_for_spawn.clone(),
                def,
                cancel_token,
                parent_id_for_spawn,
            ).await;

            // v3.5：保存 payload 到 TaskHandle.final_result（再调 mark_terminal 时 emit 给前端）
            // 只在 Ok 状态下保留 payload，Err 状态不携带 payload
            if let Ok(ref tr) = outcome {
                let mut tasks = inner.tasks.write();
                if let Some(h) = tasks.get_mut(&task_id_for_spawn) {
                    h.final_result = Some(Ok(tr.clone()));
                }
            }

            // 6.6 根据结果更新状态
            // ✅ P0-1 修复：Failed / Panicked / Cancelled 都把 error 注入 summary，
            // 否则前端 e.payload.summary 为 null，显示"未知错误"
            let (final_status, summary) = match &outcome {
                Ok(result) => {
                    tracing::info!(?task_id_for_spawn, summary = ?result.summary, "task completed");
                    (TaskStatus::Completed, Some(result.summary.clone()))
                }
                Err(FinalErr::ActionFailed(msg)) => {
                    tracing::warn!(?task_id_for_spawn, error = %msg, "task failed");
                    let summary = if msg.is_empty() {
                        "操作失败（无详细信息）".to_string()
                    } else {
                        format!("操作失败: {}", msg)
                    };
                    (TaskStatus::Failed { error: msg.clone() }, Some(summary))
                }
                Err(FinalErr::Panicked(msg)) => {
                    tracing::error!(?task_id_for_spawn, error = %msg, "task panicked");
                    let summary = format!("内部错误（任务 panic）: {}", msg);
                    (TaskStatus::Failed { error: msg.clone() }, Some(summary))
                }
                Err(FinalErr::Cancelled) => {
                    tracing::info!(?task_id_for_spawn, "task cancelled by user");
                    (TaskStatus::Cancelled, Some("操作已取消".to_string()))
                }
            };

            // 6.7 释放 permit（drop guard）
            drop(permit);

            // 6.8-6.10 终态处理
            Inner::mark_terminal(&inner, &app, &log_store, &task_id_for_spawn, final_status, summary).await;
        });

        Ok(task_id)
    }

    /// ✅ P2-1 新增：提交子任务（自动注入 parent_id）
    ///
    /// 业务用法：
    /// ```ignore
    /// let child_id = scheduler.submit_child(
    ///     make_install_torch_task(...),
    ///     parent_task_id,
    /// ).await?;
    /// ```
    ///
    /// 内部：在 TaskDef.parent_id 写入 parent_task_id 后调 `submit`。
    /// 等价于 `submit` + 手动设 `def.parent_id`，但更安全（避免忘记设）。
    pub async fn submit_child(
        &self,
        mut def: TaskDef,
        parent_id: TaskId,
    ) -> Result<TaskId, TaskError> {
        if def.parent_id.is_some() {
            tracing::warn!(
                ?parent_id,
                "submit_child 覆盖了 TaskDef 原有 parent_id（不应同时设置）"
            );
        }
        def.parent_id = Some(parent_id);
        self.submit(def).await
    }

    /// 取消任务：已完成/已取消的任务返回 Ok（幂等）
    ///
    /// 流程：
    /// 1. 优先找 cancel_token（运行中或排队中）
    /// 2. token 不在 → 检查是否已终态（幂等返回 Ok）
    /// 3. 都不在 → NotFound
    pub async fn cancel(&self, id: &TaskId) -> Result<(), TaskError> {
        // 1. 优先找 cancel_token
        if let Some(entry) = self.inner.cancel_tokens.get(id) {
            entry.cancel();
            tracing::info!(?id, "task cancel requested");
            return Ok(());
        }
        // 2. token 不在 → 检查是否已终态
        let tasks = self.inner.tasks.read();
        match tasks.get(id) {
            Some(h) => {
                if h.info.status.is_terminal() {
                    tracing::debug!(?id, status = ?h.info.status, "cancel on terminal task, idempotent ok");
                    Ok(())
                } else {
                    // 理论上不会到这（有 status 必有 token），但返回 Ok 防御
                    tracing::warn!(?id, "task not terminal but no cancel_token, defensive ok");
                    Ok(())
                }
            }
            None => Err(TaskError::NotFound(id.clone())),
        }
    }

    /// 列出所有任务快照（按 started_at 倒序，未开始的排最后）
    pub async fn list(&self) -> Vec<TaskInfo> {
        let tasks = self.inner.tasks.read();
        let mut infos: Vec<TaskInfo> = tasks.values().map(|h| h.info.clone()).collect();
        infos.sort_by(|a, b| {
            b.started_at.cmp(&a.started_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        infos
    }

    /// 查询单个任务
    pub async fn get(&self, id: &TaskId) -> Option<TaskInfo> {
        let tasks = self.inner.tasks.read();
        tasks.get(id).map(|h| h.info.clone())
    }

    /// **v3.4.1 新增**：检查是否存在指定 kind 的非终态任务（用于后端幂等守卫）
    ///
    /// 用法：调用方在 `submit` 前调用，避免重复入队。
    /// - 返回 `Some(TaskInfo)`：已有同 kind 的 queued / running 任务
    /// - 返回 `None`：可以提交
    ///
    /// ## 设计
    /// - 只过滤"未终态"（Queued / Running），Completed / Failed / Cancelled 视为可再次提交
    /// - 顺序：按 `started_at` 倒序，取最新的一条
    /// - 不阻塞、不修改状态
    pub async fn find_active_by_kind(&self, kind: &TaskKind) -> Option<TaskInfo> {
        let tasks = self.inner.tasks.read();
        tasks
            .values()
            .filter(|h| h.info.kind == *kind && !h.info.status.is_terminal())
            .map(|h| h.info.clone())
            .max_by(|a, b| {
                let ka = match a.started_at {
                    Some(t) => t,
                    None => chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap(),
                };
                let kb = match b.started_at {
                    Some(t) => t,
                    None => chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap(),
                };
                kb.cmp(&ka)
            })
    }

    /// 阻塞等待任务完成，返回结果或错误
    ///
    /// 实现：每 20ms 查询 status，终态后返回缓存结果
    pub async fn wait(&self, id: &TaskId) -> Result<TaskResult, TaskError> {
        loop {
            // 检查任务是否存在 + 缓存结果
            let cached_result: Option<Result<TaskResult, TaskError>> = {
                let tasks = self.inner.tasks.read();
                let h = tasks.get(id);
                match h {
                    None => return Err(TaskError::NotFound(id.clone())),
                    Some(h) => {
                        // 优先返回缓存的 final_result
                        if let Some(result) = &h.final_result {
                            Some(match result {
                                Ok(r) => Ok(r.clone()),
                                Err(FinalErr::ActionFailed(msg)) => Err(TaskError::ActionFailed { error: msg.clone() }),
                                Err(FinalErr::Panicked(msg)) => Err(TaskError::ActionFailed { error: msg.clone() }),
                                Err(FinalErr::Cancelled) => Err(TaskError::WaitCancelled),
                            })
                        } else {
                            // 检查状态
                            match &h.info.status {
                                TaskStatus::Completed => Some(Ok(TaskResult::new("(no summary)"))),
                                TaskStatus::Failed { error } => Some(Err(TaskError::ActionFailed { error: error.clone() })),
                                TaskStatus::Cancelled => Some(Err(TaskError::WaitCancelled)),
                                _ => None,
                            }
                        }
                    }
                }
            };

            if let Some(result) = cached_result {
                return result;
            }

            tokio::time::sleep(WAIT_POLL_INTERVAL).await;
        }
    }
}

impl Inner {
    /// 终态处理：更新 status + emit terminal + 写 LogStore + 移除 cancel_token + 淘汰
    async fn mark_terminal(
        inner: &Arc<Inner>,
        app: &Option<AppHandle>,
        log_store: &Arc<LogStoreService>,
        task_id: &TaskId,
        status: TaskStatus,
        summary: Option<String>,
    ) {
        // 1. 更新 TaskHandle
        let (info_snapshot, payload) = {
            let mut tasks = inner.tasks.write();
            if let Some(h) = tasks.get_mut(task_id) {
                h.info.status = status.clone();
                h.info.completed_at = Some(chrono::Utc::now());
                h.cancel_token = None;
                // 缓存 final_result 供 wait 调用
                let mut task_result = match &status {
                    TaskStatus::Completed => Ok(TaskResult::new(summary.clone().unwrap_or_default())),
                    TaskStatus::Failed { error } => Err(FinalErr::ActionFailed(error.clone())),
                    TaskStatus::Cancelled => Err(FinalErr::Cancelled),
                    _ => Ok(TaskResult::new("(unexpected)")),
                };
                // v3.5：保留原本 final_result 中的 payload（run_action 后已写入）
                let final_payload = h
                    .final_result
                    .as_ref()
                    .and_then(|r| r.as_ref().ok())
                    .and_then(|r| r.payload.clone());
                if let Ok(ref mut tr) = task_result.as_mut() {
                    if tr.payload.is_none() {
                        tr.payload = final_payload;
                    }
                }
                h.final_result = Some(match &task_result {
                    Ok(tr) => Ok(tr.clone()),
                    Err(FinalErr::ActionFailed(msg)) => Err(FinalErr::ActionFailed(msg.clone())),
                    Err(FinalErr::Cancelled) => Err(FinalErr::Cancelled),
                    Err(FinalErr::Panicked(msg)) => Err(FinalErr::Panicked(msg.clone())),
                });
                // v3.5：从 final_result 取 payload 用于 emit
                let emit_payload = h
                    .final_result
                    .as_ref()
                    .and_then(|r| r.as_ref().ok())
                    .and_then(|r| r.payload.clone());
                (Some(h.info.clone()), emit_payload)
            } else {
                (None, None)
            }
        };

        // 2. 移除 cancel_token（cancel 幂等）
        inner.cancel_tokens.remove(task_id);

        // 3. emit terminal event
        if let Some(info) = &info_snapshot {
            emit_terminal(
                app,
                &info.id,
                info.status.as_str(),
                summary.as_deref(),
                payload.as_ref(),
            );
        }

        // 4. 写 LogStore（异步，失败仅 warn，不阻塞终态返回）
        if let Some(info) = &info_snapshot {
            if let Err(e) = history::record(log_store, info).await {
                tracing::warn!(?task_id, error = %e, "record_task_history failed, history lost");
            }
        }

        // 5. 淘汰机制
        Self::maybe_evict(inner).await;
    }

    /// 终态任务淘汰（保留最近 MAX_TERMINAL_KEEP 条）
    async fn maybe_evict(inner: &Arc<Inner>) {
        let mut tasks = inner.tasks.write();
        let terminal: Vec<(TaskId, Option<chrono::DateTime<chrono::Utc>>)> = tasks
            .iter()
            .filter(|(_, h)| h.info.status.is_terminal())
            .map(|(id, h)| (id.clone(), h.info.completed_at))
            .collect();

        if terminal.len() > MAX_TERMINAL_KEEP {
            let mut sorted = terminal;
            sorted.sort_by_key(|(_, t)| *t);
            let evict_count = sorted.len() - MAX_TERMINAL_KEEP;
            for (id, _) in sorted.iter().take(evict_count) {
                tasks.remove(id);
                tracing::debug!(?id, "evicted terminal task from memory");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_kind_default_priority() {
        assert_eq!(TaskKind::Checkout.default_priority(), TaskPriority::High);
        assert_eq!(TaskKind::InstallTorch.default_priority(), TaskPriority::High);
        assert_eq!(TaskKind::PluginInstall.default_priority(), TaskPriority::Normal);
        assert_eq!(TaskKind::ScanModels.default_priority(), TaskPriority::Low);
    }

    #[test]
    fn test_task_status_is_terminal() {
        assert!(TaskStatus::Completed.is_terminal());
        assert!(TaskStatus::Failed { error: "x".into() }.is_terminal());
        assert!(TaskStatus::Cancelled.is_terminal());
        assert!(!TaskStatus::Queued.is_terminal());
        assert!(!TaskStatus::Running { progress: 50 }.is_terminal());
    }

    #[test]
    fn test_task_kind_as_str() {
        assert_eq!(TaskKind::CloneRepo.as_str(), "clone_repo");
        assert_eq!(TaskKind::FetchTags.as_str(), "fetch_tags");
        // v3.4
        assert_eq!(TaskKind::StartComfyUI.as_str(), "start_comfyui");
        assert_eq!(TaskKind::Custom.as_str(), "custom");
    }

    #[test]
    fn test_task_handle_new() {
        let (handle, token) = TaskHandle::new(
            "t1".to_string(),
            TaskKind::Custom,
            "test".to_string(),
            TaskPriority::Normal,
            None,
        );
        assert_eq!(handle.info.id, "t1");
        assert!(matches!(handle.info.status, TaskStatus::Queued));
        assert!(handle.info.started_at.is_none());
        assert!(!token.is_cancelled());
        token.cancel();
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn test_maybe_evict_keeps_max() {
        let inner = Arc::new(Inner {
            tasks: PlRwLock::new(HashMap::new()),
            cancel_tokens: DashMap::new(),
            queued_count: PlRwLock::new(0),
        });
        {
            let mut t = inner.tasks.write();
            for i in 0..(MAX_TERMINAL_KEEP + 50) {
                let (mut h, _) = TaskHandle::new(
                    format!("t{}", i),
                    TaskKind::Custom,
                    format!("task {}", i),
                    TaskPriority::Normal,
                    None,
                );
                h.info.status = TaskStatus::Completed;
                h.info.completed_at = Some(chrono::Utc::now() - chrono::Duration::seconds(i as i64));
                t.insert(format!("t{}", i), h);
            }
        }
        Inner::maybe_evict(&inner).await;
        let t = inner.tasks.read();
        assert_eq!(t.len(), MAX_TERMINAL_KEEP);
    }
}
