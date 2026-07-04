//! LogStore 数据访问层
//!
//! 设计模式：
//! - **Repository**：每张表一个 repository，封装 SQL 细节
//! - **DAO**：分离数据访问逻辑与业务逻辑
//!
//! 详见 `PR/03-模块设计/09-LogStore.md §3 接口签名` 和 `§5 数据流`

use crate::log_store::LogStoreError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePool;

// ───────────────────────── 值对象 ─────────────────────────

/// 日志级别
///
/// 序列化为小写字符串（`trace` / `debug` / `info` / `warn` / `error`），
/// 与 SQLite `logs.level` 字段保持一致
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }

    /// 从字符串解析（解析失败返回 None）
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "trace" => Some(Self::Trace),
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warn" => Some(Self::Warn),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// 单条日志（与 logs 表对应）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub level: LogLevel,
    /// 来源模块名（如 "ProcessLauncher" / "CoreManager" / "comfyui"）
    pub source: String,
    pub message: String,
}

/// 日志查询过滤条件
///
/// 所有字段可选，默认按时间倒序、LIMIT 100
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LogQueryFilter {
    pub level: Option<LogLevel>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub source: Option<String>,
    /// 关键词模糊匹配 message（LIKE %keyword%）
    pub keyword: Option<String>,
    /// 0 表示使用默认值 100
    pub limit: usize,
    pub offset: usize,
}

/// 任务历史记录（审计用）
///
/// 字段 `kind` 取值：
/// - `clone_repo` / `checkout` / `pull`
/// - `install_plugin` / `uninstall_plugin` / `update_plugin`
/// - `install_python` / `create_venv` / `switch_torch` / `rebuild_venv`
/// - `start_process` / `stop_process`
///
/// 字段 `status` 取值：`success` / `failed` / `cancelled`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskHistoryRecord {
    pub kind: String,
    pub name: String,
    pub status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

// ───────────────────────── 日志 Repository ─────────────────────────

/// 日志 Repository（`logs` + `tags_cache` 表）
///
/// 生命周期绑定到 `LogStoreService`，通过 `&SqlitePool` 共享连接池
pub struct LogRepository<'a> {
    pub(crate) pool: &'a SqlitePool,
}

impl<'a> LogRepository<'a> {
    /// 批量写入日志（单事务）
    ///
    /// 幂等：每条日志独立，重复写只是多条记录
    pub async fn append_batch(&self, entries: &[LogEntry]) -> Result<(), LogStoreError> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for e in entries {
            sqlx::query(
                "INSERT INTO logs(timestamp, level, source, message) VALUES (?, ?, ?, ?)",
            )
            .bind(e.timestamp.to_rfc3339())
            .bind(e.level.as_str())
            .bind(&e.source)
            .bind(&e.message)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        tracing::trace!(count = entries.len(), "appended logs batch");
        Ok(())
    }

    /// 追加单条日志
    pub async fn append(&self, entry: LogEntry) -> Result<(), LogStoreError> {
        self.append_batch(&[entry]).await
    }

    /// 按条件查询历史日志（命中索引，<100ms）
    ///
    /// 返回结果按时间倒序
    pub async fn query(&self, filter: &LogQueryFilter) -> Result<Vec<LogEntry>, LogStoreError> {
        let mut sql = String::from("SELECT timestamp, level, source, message FROM logs WHERE 1=1");
        if filter.level.is_some() {
            sql.push_str(" AND level = ?");
        }
        if filter.since.is_some() {
            sql.push_str(" AND timestamp >= ?");
        }
        if filter.until.is_some() {
            sql.push_str(" AND timestamp <= ?");
        }
        if filter.source.is_some() {
            sql.push_str(" AND source = ?");
        }
        if filter.keyword.is_some() {
            sql.push_str(" AND message LIKE ?");
        }
        sql.push_str(" ORDER BY timestamp DESC LIMIT ? OFFSET ?");

        let mut q = sqlx::query_as::<_, LogRow>(&sql);
        if let Some(lvl) = filter.level {
            q = q.bind(lvl.as_str());
        }
        if let Some(since) = filter.since {
            q = q.bind(since.to_rfc3339());
        }
        if let Some(until) = filter.until {
            q = q.bind(until.to_rfc3339());
        }
        if let Some(src) = &filter.source {
            q = q.bind(src);
        }
        if let Some(kw) = &filter.keyword {
            q = q.bind(format!("%{}%", kw));
        }
        let limit = if filter.limit == 0 { 100 } else { filter.limit };
        q = q.bind(limit as i64).bind(filter.offset as i64);

        let rows = q.fetch_all(self.pool).await?;
        Ok(rows.into_iter().map(LogEntry::from).collect())
    }

    /// 取最近 N 行日志（<10ms）
    ///
    /// 按 id DESC 取最近 N 条，返回时调整为时间正序展示
    pub async fn tail(&self, lines: usize) -> Result<Vec<LogEntry>, LogStoreError> {
        let n = if lines == 0 { 100 } else { lines };
        let rows = sqlx::query_as::<_, LogRow>(
            "SELECT timestamp, level, source, message FROM logs ORDER BY id DESC LIMIT ?",
        )
        .bind(n as i64)
        .fetch_all(self.pool)
        .await?;
        let mut entries: Vec<LogEntry> = rows.into_iter().map(LogEntry::from).collect();
        entries.reverse();
        Ok(entries)
    }

    /// 清理 N 天前的日志，返回删除行数
    ///
    /// 完全幂等：DELETE WHERE timestamp < ?，多次执行结果一致
    pub async fn cleanup_old(&self, days: u32) -> Result<u64, LogStoreError> {
        let cutoff = Utc::now() - chrono::Duration::days(days as i64);
        let result = sqlx::query("DELETE FROM logs WHERE timestamp < ?")
            .bind(cutoff.to_rfc3339())
            .execute(self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    /// 清空所有日志（调试 / 用户手动清理场景）
    pub async fn clear_all(&self) -> Result<(), LogStoreError> {
        sqlx::query("DELETE FROM logs").execute(self.pool).await?;
        Ok(())
    }

    /// 缓存 tags（覆盖写单行，幂等）
    ///
    /// `tags_json` 由 CoreManager 在 fetch_tags 成功后调用，
    /// 传入 `serde_json::to_string(&tags)` 结果
    pub async fn cache_tags(&self, tags_json: &str) -> Result<(), LogStoreError> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT OR REPLACE INTO tags_cache(id, tags_json, fetched_at) VALUES (1, ?, ?)",
        )
        .bind(tags_json)
        .bind(now)
        .execute(self.pool)
        .await?;
        tracing::info!(bytes = tags_json.len(), "tags cached");
        Ok(())
    }

    /// 读取已缓存的 tags（启动时秒级展示）
    ///
    /// 返回 `(tags_json, fetched_at_iso8601)`
    pub async fn load_cached_tags(&self) -> Result<Option<(String, String)>, LogStoreError> {
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT tags_json, fetched_at FROM tags_cache WHERE id = 1")
                .fetch_optional(self.pool)
                .await?;
        if row.is_some() {
            tracing::debug!("loaded cached tags");
        }
        Ok(row)
    }

    /// 失效 tags 缓存（强制下次 fetch 后重建）
    ///
    /// 幂等：DELETE 后再 DELETE 为空操作
    pub async fn invalidate_tags_cache(&self) -> Result<(), LogStoreError> {
        sqlx::query("DELETE FROM tags_cache WHERE id = 1")
            .execute(self.pool)
            .await?;
        Ok(())
    }
}

// ───────────────────────── 任务历史 Repository ─────────────────────────

/// 任务历史 Repository（`task_history` 表）
pub struct TaskRepository<'a> {
    pub(crate) pool: &'a SqlitePool,
}

impl<'a> TaskRepository<'a> {
    /// 记录任务历史（不幂等：每次新增一条审计记录）
    pub async fn append(&self, record: TaskHistoryRecord) -> Result<(), LogStoreError> {
        sqlx::query(
            "INSERT INTO task_history(kind, name, status, started_at, completed_at, error)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&record.kind)
        .bind(&record.name)
        .bind(&record.status)
        .bind(record.started_at.map(|t| t.to_rfc3339()))
        .bind(record.completed_at.map(|t| t.to_rfc3339()))
        .bind(&record.error)
        .execute(self.pool)
        .await?;
        tracing::info!(
            kind = %record.kind,
            name = %record.name,
            status = %record.status,
            "task history recorded"
        );
        Ok(())
    }

    /// 查询任务历史（按 id DESC 倒序）
    pub async fn query_history(
        &self,
        limit: usize,
    ) -> Result<Vec<TaskHistoryRecord>, LogStoreError> {
        let n = if limit == 0 { 100 } else { limit };
        let rows: Vec<TaskRow> = sqlx::query_as(
            "SELECT kind, name, status, started_at, completed_at, error
             FROM task_history ORDER BY id DESC LIMIT ?",
        )
        .bind(n as i64)
        .fetch_all(self.pool)
        .await?;
        Ok(rows.into_iter().map(TaskHistoryRecord::from).collect())
    }

    /// 清理 N 天前的任务历史，返回删除行数
    pub async fn cleanup_old(&self, days: u32) -> Result<u64, LogStoreError> {
        let cutoff = Utc::now() - chrono::Duration::days(days as i64);
        let result = sqlx::query("DELETE FROM task_history WHERE started_at < ?")
            .bind(cutoff.to_rfc3339())
            .execute(self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

// ───────────────────────── 内部行类型 ─────────────────────────

#[derive(sqlx::FromRow)]
struct LogRow {
    timestamp: String,
    level: String,
    source: String,
    message: String,
}

impl From<LogRow> for LogEntry {
    fn from(r: LogRow) -> Self {
        let timestamp = DateTime::parse_from_rfc3339(&r.timestamp)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        let level = LogLevel::parse(&r.level).unwrap_or(LogLevel::Info);
        Self {
            timestamp,
            level,
            source: r.source,
            message: r.message,
        }
    }
}

#[derive(sqlx::FromRow)]
struct TaskRow {
    kind: String,
    name: String,
    status: String,
    started_at: Option<String>,
    completed_at: Option<String>,
    error: Option<String>,
}

impl From<TaskRow> for TaskHistoryRecord {
    fn from(r: TaskRow) -> Self {
        let parse = |s: Option<String>| {
            s.and_then(|t| DateTime::parse_from_rfc3339(&t).ok().map(|d| d.with_timezone(&Utc)))
        };
        Self {
            kind: r.kind,
            name: r.name,
            status: r.status,
            started_at: parse(r.started_at),
            completed_at: parse(r.completed_at),
            error: r.error,
        }
    }
}

// ───────────────────────── 单元测试 ─────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
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

    fn make_entry(level: LogLevel, source: &str, msg: &str) -> LogEntry {
        LogEntry {
            timestamp: Utc::now(),
            level,
            source: source.to_string(),
            message: msg.to_string(),
        }
    }

    #[tokio::test]
    async fn test_append_and_query() {
        let pool = setup().await;
        let repo = LogRepository { pool: &pool };

        repo.append(make_entry(LogLevel::Info, "test", "msg1"))
            .await
            .unwrap();
        repo.append(make_entry(LogLevel::Warn, "test", "msg2"))
            .await
            .unwrap();
        repo.append(make_entry(LogLevel::Error, "test", "msg3"))
            .await
            .unwrap();

        let result = repo.query(&LogQueryFilter::default()).await.unwrap();
        assert_eq!(result.len(), 3);
    }

    #[tokio::test]
    async fn test_append_batch() {
        let pool = setup().await;
        let repo = LogRepository { pool: &pool };

        let entries: Vec<LogEntry> = (0..100)
            .map(|i| make_entry(LogLevel::Info, "batch", &format!("entry {}", i)))
            .collect();
        repo.append_batch(&entries).await.unwrap();

        let result = repo.tail(100).await.unwrap();
        assert_eq!(result.len(), 100);
    }

    #[tokio::test]
    async fn test_query_by_level() {
        let pool = setup().await;
        let repo = LogRepository { pool: &pool };

        repo.append(make_entry(LogLevel::Info, "test", "info msg"))
            .await
            .unwrap();
        repo.append(make_entry(LogLevel::Error, "test", "err msg"))
            .await
            .unwrap();
        repo.append(make_entry(LogLevel::Error, "test", "err msg 2"))
            .await
            .unwrap();

        let filter = LogQueryFilter {
            level: Some(LogLevel::Error),
            ..Default::default()
        };
        let result = repo.query(&filter).await.unwrap();
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|e| e.level == LogLevel::Error));
    }

    #[tokio::test]
    async fn test_query_by_keyword() {
        let pool = setup().await;
        let repo = LogRepository { pool: &pool };

        repo.append(make_entry(LogLevel::Info, "test", "ComfyUI starting"))
            .await
            .unwrap();
        repo.append(make_entry(LogLevel::Info, "test", "torch loaded"))
            .await
            .unwrap();

        let filter = LogQueryFilter {
            keyword: Some("torch".to_string()),
            ..Default::default()
        };
        let result = repo.query(&filter).await.unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].message.contains("torch"));
    }

    #[tokio::test]
    async fn test_query_by_source() {
        let pool = setup().await;
        let repo = LogRepository { pool: &pool };

        repo.append(make_entry(LogLevel::Info, "CoreManager", "core msg"))
            .await
            .unwrap();
        repo.append(make_entry(LogLevel::Info, "ProcessLauncher", "proc msg"))
            .await
            .unwrap();

        let filter = LogQueryFilter {
            source: Some("CoreManager".to_string()),
            ..Default::default()
        };
        let result = repo.query(&filter).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source, "CoreManager");
    }

    #[tokio::test]
    async fn test_tail_limit() {
        let pool = setup().await;
        let repo = LogRepository { pool: &pool };

        for i in 0..1000 {
            repo.append(make_entry(LogLevel::Info, "test", &format!("entry {}", i)))
                .await
                .unwrap();
        }

        let result = repo.tail(500).await.unwrap();
        assert_eq!(result.len(), 500);
    }

    #[tokio::test]
    async fn test_clear_all() {
        let pool = setup().await;
        let repo = LogRepository { pool: &pool };

        repo.append(make_entry(LogLevel::Info, "test", "msg"))
            .await
            .unwrap();
        repo.clear_all().await.unwrap();

        let result = repo.query(&LogQueryFilter::default()).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_cleanup_old_idempotent() {
        let pool = setup().await;
        let repo = LogRepository { pool: &pool };

        // 写入 8 天前的日志
        let mut old_entry = make_entry(LogLevel::Info, "test", "old msg");
        old_entry.timestamp = Utc::now() - chrono::Duration::days(8);
        repo.append(old_entry).await.unwrap();

        // 写入今天的日志
        repo.append(make_entry(LogLevel::Info, "test", "new msg"))
            .await
            .unwrap();

        let deleted = repo.cleanup_old(7).await.unwrap();
        assert_eq!(deleted, 1);

        // 第二次清理，应返回 0（幂等）
        let deleted_again = repo.cleanup_old(7).await.unwrap();
        assert_eq!(deleted_again, 0);

        let result = repo.query(&LogQueryFilter::default()).await.unwrap();
        assert_eq!(result.len(), 1);
    }

    #[tokio::test]
    async fn test_cache_tags_overwrite() {
        let pool = setup().await;
        let repo = LogRepository { pool: &pool };

        // 初始为空
        let cached = repo.load_cached_tags().await.unwrap();
        assert!(cached.is_none());

        // 第一次缓存
        repo.cache_tags(r#"[]"#).await.unwrap();
        let (json1, _) = repo.load_cached_tags().await.unwrap().unwrap();
        assert_eq!(json1, "[]");

        // 覆盖写
        repo.cache_tags(r#"[{"name":"v0.2.0"}]"#)
            .await
            .unwrap();
        let (json2, _) = repo.load_cached_tags().await.unwrap().unwrap();
        assert_eq!(json2, r#"[{"name":"v0.2.0"}]"#);
    }

    #[tokio::test]
    async fn test_invalidate_tags_cache() {
        let pool = setup().await;
        let repo = LogRepository { pool: &pool };

        repo.cache_tags("[]").await.unwrap();
        repo.invalidate_tags_cache().await.unwrap();

        let cached = repo.load_cached_tags().await.unwrap();
        assert!(cached.is_none());

        // 重复失效不应报错（幂等）
        repo.invalidate_tags_cache().await.unwrap();
    }

    #[tokio::test]
    async fn test_record_task_history_not_idempotent() {
        let pool = setup().await;
        let repo = TaskRepository { pool: &pool };

        let record = TaskHistoryRecord {
            kind: "checkout".to_string(),
            name: "v0.2.0".to_string(),
            status: "success".to_string(),
            started_at: Some(Utc::now()),
            completed_at: Some(Utc::now()),
            error: None,
        };

        repo.append(record.clone()).await.unwrap();
        repo.append(record).await.unwrap(); // 重复记录

        let history = repo.query_history(100).await.unwrap();
        assert_eq!(history.len(), 2);
    }

    #[tokio::test]
    async fn test_query_task_history_order() {
        let pool = setup().await;
        let repo = TaskRepository { pool: &pool };

        // 按时间间隔写入 3 条
        for i in 0..3 {
            let mut r = TaskHistoryRecord {
                kind: format!("task_{}", i),
                name: "test".to_string(),
                status: "success".to_string(),
                started_at: Some(Utc::now() + chrono::Duration::seconds(i)),
                completed_at: None,
                error: None,
            };
            // 调整时间戳确保顺序
            r.started_at = Some(Utc::now() + chrono::Duration::seconds(i));
            repo.append(r).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let history = repo.query_history(100).await.unwrap();
        assert_eq!(history.len(), 3);
        // 按 id DESC，最新写入的在前面
        assert_eq!(history[0].kind, "task_2");
        assert_eq!(history[2].kind, "task_0");
    }
}
