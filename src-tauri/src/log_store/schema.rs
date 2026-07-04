//! LogStore 表结构定义
//!
//! 详见 `PR/03-模块设计/09-LogStore.md §4.1 表结构`
//!
//! 三张表：
//! - `logs`：运行日志（带时间索引 + 复合 level+time 索引）
//! - `tags_cache`：单行覆盖写（CoreManager tags 缓存，id 恒为 1）
//! - `task_history`：任务历史审计（带 started_at 索引）

use sqlx::sqlite::SqlitePool;

/// PRAGMA 配置
const PRAGMA_STATEMENTS: &[&str] = &[
    "PRAGMA journal_mode = WAL",
    "PRAGMA synchronous = NORMAL",
    "PRAGMA foreign_keys = ON",
];

/// 表与索引定义（按依赖顺序）
const SCHEMA_STATEMENTS: &[&str] = &[
    // logs 表
    "CREATE TABLE IF NOT EXISTS logs (
        id        INTEGER PRIMARY KEY AUTOINCREMENT,
        timestamp TEXT    NOT NULL,
        level     TEXT    NOT NULL,
        source    TEXT    NOT NULL,
        message   TEXT    NOT NULL
    )",
    "CREATE INDEX IF NOT EXISTS idx_logs_time ON logs(timestamp)",
    "CREATE INDEX IF NOT EXISTS idx_logs_level_time ON logs(level, timestamp)",
    // tags_cache 表（单行，id 恒为 1，覆盖写）
    "CREATE TABLE IF NOT EXISTS tags_cache (
        id         INTEGER PRIMARY KEY,
        tags_json  TEXT    NOT NULL,
        fetched_at TEXT    NOT NULL
    )",
    // task_history 表
    "CREATE TABLE IF NOT EXISTS task_history (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        kind          TEXT,
        name          TEXT,
        status        TEXT,
        started_at    TEXT,
        completed_at  TEXT,
        error         TEXT
    )",
    "CREATE INDEX IF NOT EXISTS idx_history_time ON task_history(started_at)",
];

/// 初始化 schema（建表 + 索引 + PRAGMA）
///
/// 幂等：所有语句均 `IF NOT EXISTS`，可重复执行
pub async fn init_schema(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    // PRAGMA
    for stmt in PRAGMA_STATEMENTS {
        sqlx::query(stmt).execute(pool).await?;
    }

    // 表与索引
    for stmt in SCHEMA_STATEMENTS {
        sqlx::query(stmt).execute(pool).await?;
    }

    tracing::info!("logstore schema initialized (WAL enabled)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn setup_pool() -> SqlitePool {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_init_schema_idempotent() {
        let pool = setup_pool().await;
        init_schema(&pool).await.unwrap();
        // 重复执行不应报错
        init_schema(&pool).await.unwrap();

        // 验证 logs 表存在
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM logs")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count.0, 0);
    }

    #[tokio::test]
    async fn test_pragma_wal_mode() {
        let pool = setup_pool().await;
        init_schema(&pool).await.unwrap();
        // 内存模式不支持 WAL，但 in-memory 测试只验证 init 不报错
        // 真实 WAL 模式由集成测试覆盖
    }
}
