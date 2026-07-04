//! 7 天日志清理后台循环
//!
//! - 启动时立即清理一次（处理上次运行期间累积的过期日志）
//! - 每 24h 定时清理
//! - 通过 `CancellationToken` 优雅停止
//!
//! 详见 `PR/03-模块设计/09-LogStore.md §4.4 自动清理调度`

use sqlx::sqlite::SqlitePool;
use tokio_util::sync::CancellationToken;

use crate::log_store::repository::{LogRepository, TaskRepository};
use crate::log_store::LogStoreError;

/// 日志保留天数
const RETENTION_DAYS: u32 = 7;

/// 清理循环间隔（24 小时）
const CLEANUP_INTERVAL_SECS: u64 = 86400;

/// 启动时清理一次 + 每 24h 定时清理
///
/// 在 `LogStoreService::new` 中通过 `tokio::spawn` 启动，
/// 在 `LogStoreService::shutdown` 中通过 cancel token 取消
pub async fn run_retention_loop(pool: SqlitePool, cancel: CancellationToken) {
    tracing::info!("logstore retention loop started");

    // 启动即清理一次（处理上次运行期间累积的过期日志）
    match cleanup_7days(&pool).await {
        Ok((logs, tasks)) => {
            if logs > 0 || tasks > 0 {
                tracing::info!(deleted_logs = logs, deleted_tasks = tasks, "startup cleanup ok");
            }
        }
        Err(e) => tracing::warn!(?e, "startup cleanup failed"),
    }

    let mut interval = tokio::time::interval(std::time::Duration::from_secs(CLEANUP_INTERVAL_SECS));
    // 跳过第一次 tick（启动时已清理过）
    interval.tick().await;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!("logstore retention loop cancelled");
                break;
            }
            _ = interval.tick() => {
                match cleanup_7days(&pool).await {
                    Ok((logs, tasks)) => {
                        tracing::info!(deleted_logs = logs, deleted_tasks = tasks, "scheduled cleanup ok");
                    }
                    Err(e) => tracing::warn!(?e, "scheduled cleanup failed"),
                }
            }
        }
    }
}

/// 清理 7 天前的日志与任务历史
///
/// 完全幂等：DELETE WHERE timestamp < ?，多次执行结果一致
pub async fn cleanup_7days(pool: &SqlitePool) -> Result<(u64, u64), LogStoreError> {
    let logs_repo = LogRepository { pool };
    let tasks_repo = TaskRepository { pool };
    let logs = logs_repo.cleanup_old(RETENTION_DAYS).await?;
    let tasks = tasks_repo.cleanup_old(RETENTION_DAYS).await?;
    Ok((logs, tasks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_store::repository::{LogEntry, LogLevel, TaskHistoryRecord};
    use chrono::Utc;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn setup() -> SqlitePool {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        crate::log_store::schema::init_schema(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn test_cleanup_7days_removes_old_records() {
        let pool = setup().await;
        let logs_repo = LogRepository { pool: &pool };
        let tasks_repo = TaskRepository { pool: &pool };

        // 写入 8 天前的日志
        let mut old_log = LogEntry {
            timestamp: Utc::now(),
            level: LogLevel::Info,
            source: "test".to_string(),
            message: "old".to_string(),
        };
        old_log.timestamp = Utc::now() - chrono::Duration::days(8);
        logs_repo.append(old_log).await.unwrap();

        // 写入今天的日志
        logs_repo
            .append(LogEntry {
                timestamp: Utc::now(),
                level: LogLevel::Info,
                source: "test".to_string(),
                message: "new".to_string(),
            })
            .await
            .unwrap();

        // 写入 8 天前的任务历史
        tasks_repo
            .append(TaskHistoryRecord {
                kind: "checkout".to_string(),
                name: "v0.1.0".to_string(),
                status: "success".to_string(),
                started_at: Some(Utc::now() - chrono::Duration::days(8)),
                completed_at: Some(Utc::now() - chrono::Duration::days(8)),
                error: None,
            })
            .await
            .unwrap();

        let (logs, tasks) = cleanup_7days(&pool).await.unwrap();
        assert_eq!(logs, 1);
        assert_eq!(tasks, 1);

        // 验证今天的数据仍在
        let logs_remaining = logs_repo.tail(100).await.unwrap();
        assert_eq!(logs_remaining.len(), 1);

        let tasks_remaining = tasks_repo.query_history(100).await.unwrap();
        assert_eq!(tasks_remaining.len(), 0);
    }

    #[tokio::test]
    async fn test_cleanup_7days_idempotent() {
        let pool = setup().await;

        let (logs1, tasks1) = cleanup_7days(&pool).await.unwrap();
        assert_eq!(logs1, 0);
        assert_eq!(tasks1, 0);

        // 重复执行不应报错且不删除任何记录
        let (logs2, tasks2) = cleanup_7days(&pool).await.unwrap();
        assert_eq!(logs2, 0);
        assert_eq!(tasks2, 0);
    }
}
