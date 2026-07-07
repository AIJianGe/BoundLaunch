//! 任务历史持久化（写入 LogStore）
//!
//! 详见 `PR/03-模块设计/08-TaskScheduler.md §8.2 LogStore 历史持久化`
//!
//! ## 字段映射
//! | TaskScheduler 内部 | LogStore.TaskHistoryRecord |
//! |---|---|
//! | TaskKind.as_str() | kind |
//! | TaskInfo.name | name |
//! | TaskStatus.as_str() | status |
//! | TaskInfo.started_at | started_at |
//! | TaskInfo.completed_at | completed_at |
//! | Failed{error} / Cancelled 时填入 | error |
//!
//! 注意：当前 LogStore schema 无 `summary` 字段；TaskResult.summary 仅在内存 TaskInfo 中保留，
//! 前端通过 `task_list` / `task_get` 查询。LogStore 仅记录审计级字段。


use chrono::{DateTime, Utc};

use crate::log_store::{LogStoreError, LogStoreService, TaskHistoryRecord};

use super::models::{TaskInfo, TaskKind, TaskStatus};

/// 写入任务历史记录
///
/// 失败时仅返回 Err，调用方决定是否 warn（设计上不阻塞终态返回）。
///
/// **v3.10 增强**：当任务是 Failed 状态时，**同时**写一条 ERROR 级日志到 LogStore，
/// 这样 ErrorPanel 顶部 + LogStore 查询都能看到（不依赖 task_history 审计表的 stderr 字段）。
pub(crate) async fn record(
    log_store: &LogStoreService,
    info: &TaskInfo,
) -> Result<(), LogStoreError> {
    let record = TaskHistoryRecord {
        kind: info.kind.as_str().to_string(),
        name: info.name.clone(),
        status: info.status.as_str().to_string(),
        started_at: info.started_at,
        completed_at: info.completed_at,
        error: extract_error(&info.status),
    };
    log_store.tasks().append(record).await?;

    // v3.10：Failed 任务额外写一条 ERROR 日志（让 ErrorPanel 实时刷新）
    if let TaskStatus::Failed { error } = &info.status {
        let message = format!("任务失败: {} ({})", info.name, info.kind.as_str());
        log_store.log_business_error(
            crate::log_store::repository::LogLevel::Error,
            &format!("task:{}", info.kind.as_str()),
            &format!("{}\n\n{}", message, error),
        );
    }

    Ok(())
}

/// 从状态中提取错误信息（仅 Failed 填充，其余为 None）
fn extract_error(status: &TaskStatus) -> Option<String> {
    match status {
        TaskStatus::Failed { error } => Some(error.clone()),
        TaskStatus::Cancelled => Some("cancelled".to_string()),
        _ => None,
    }
}

/// 计算任务耗时（毫秒）
///
/// 用于日志埋点，未持久化到 LogStore（当前 schema 无 duration_ms 字段）
pub(crate) fn duration_ms(info: &TaskInfo) -> Option<i64> {
    match (info.started_at, info.completed_at) {
        (Some(s), Some(c)) => Some((c - s).num_milliseconds()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_error_failed() {
        let status = TaskStatus::Failed {
            error: "boom".to_string(),
        };
        assert_eq!(extract_error(&status), Some("boom".to_string()));
    }

    #[test]
    fn test_extract_error_cancelled() {
        let status = TaskStatus::Cancelled;
        assert_eq!(extract_error(&status), Some("cancelled".to_string()));
    }

    #[test]
    fn test_extract_error_completed() {
        let status = TaskStatus::Completed;
        assert_eq!(extract_error(&status), None);
    }

    #[test]
    fn test_duration_ms() {
        let info = TaskInfo {
            id: "t1".to_string(),
            kind: TaskKind::Custom,
            name: "test".to_string(),
            priority: crate::task_scheduler::TaskPriority::Normal,
            status: TaskStatus::Completed,
            started_at: Some(DateTime::parse_from_rfc3339("2026-07-04T00:00:00Z").unwrap().with_timezone(&Utc)),
            completed_at: Some(DateTime::parse_from_rfc3339("2026-07-04T00:00:05Z").unwrap().with_timezone(&Utc)),
            parent_id: None,
        };
        assert_eq!(duration_ms(&info), Some(5000));
    }

    #[tokio::test]
    async fn test_record_to_logstore() {
        // 集成测试：真实 LogStore
        let store = LogStoreService::new(None).await.unwrap();
        let info = TaskInfo {
            id: "t1".to_string(),
            kind: TaskKind::Custom,
            name: "测试任务".to_string(),
            priority: crate::task_scheduler::TaskPriority::Normal,
            status: TaskStatus::Completed,
            started_at: Some(Utc::now()),
            completed_at: Some(Utc::now()),
            parent_id: None,
        };
        let result = record(&store, &info).await;
        assert!(result.is_ok());

        // 查询验证
        let history = store.tasks().query_history(10).await.unwrap();
        let last = history.last().unwrap();
        assert_eq!(last.kind, "custom");
        assert_eq!(last.name, "测试任务");
        assert_eq!(last.status, "completed");
        assert!(last.error.is_none());
    }
}
