//! LogStore 模块 - SQLite 持久化日志与任务历史
//!
//! 设计模式：
//! - Repository：数据访问层抽象
//! - DAO：每张表一个 repository
//!
//! 详见 `PR/03-模块设计/09-LogStore.md`

pub mod repository;
pub mod retention;
pub mod schema;

pub use repository::{LogRepository, TaskRepository, TaskHistoryRecord};

use crate::common::paths;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::path::PathBuf;
use std::str::FromStr;

/// LogStore 错误类型
#[derive(Debug, thiserror::Error, serde::Serialize)]
pub enum LogStoreError {
    #[error("数据库初始化失败: {0}")]
    InitFailed(String),
    #[error("数据库查询失败: {0}")]
    QueryFailed(String),
    #[error("数据库写入失败: {0}")]
    InsertFailed(String),
}

impl From<sqlx::Error> for LogStoreError {
    fn from(e: sqlx::Error) -> Self {
        Self::QueryFailed(e.to_string())
    }
}

/// LogStore 服务
///
/// 包装 SQLite 连接池，提供 logs / task_history 表的访问
pub struct LogStoreService {
    pool: SqlitePool,
    /// 7 天清理任务的取消令牌
    retention_cancel: tokio_util::sync::CancellationToken,
}

impl LogStoreService {
    /// 初始化 LogStore
    ///
    /// - 自动创建数据库文件
    /// - 自动建表（IF NOT EXISTS）
    /// - 启动后台 7 天清理任务
    pub async fn new(db_path: Option<PathBuf>) -> Result<Self, LogStoreError> {
        let path = db_path.unwrap_or_else(paths::log_db_path);

        // 确保父目录存在
        if let Some(parent) = path.parent() {
            paths::ensure_dir(parent)
                .await
                .map_err(|e| LogStoreError::InitFailed(e.to_string()))?;
        }

        // 连接池配置：开启 WAL 模式
        let conn_str = format!("sqlite://{}?mode=rwc", path.display());
        let options = SqliteConnectOptions::from_str(&conn_str)
            .map_err(|e| LogStoreError::InitFailed(e.to_string()))?
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .create_if_missing(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .map_err(|e| LogStoreError::InitFailed(e.to_string()))?;

        // 建表
        schema::init_schema(&pool)
            .await
            .map_err(|e| LogStoreError::InitFailed(e.to_string()))?;

        // 启动后台 7 天清理任务
        let retention_cancel = tokio_util::sync::CancellationToken::new();
        let retention_pool = pool.clone();
        let retention_token = retention_cancel.clone();
        tokio::spawn(async move {
            retention::run_retention_loop(retention_pool, retention_token).await;
        });

        Ok(Self {
            pool,
            retention_cancel,
        })
    }

    /// 获取连接池（内部使用）
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// 日志 repository
    pub fn logs(&self) -> LogRepository<'_> {
        LogRepository { pool: &self.pool }
    }

    /// 任务历史 repository
    pub fn tasks(&self) -> TaskRepository<'_> {
        TaskRepository { pool: &self.pool }
    }

    /// 优雅停止（取消后台清理）
    pub fn shutdown(&self) {
        self.retention_cancel.cancel();
    }
}

/// LogStoreError → AppError 的转换
impl From<LogStoreError> for crate::error::AppError {
    fn from(e: LogStoreError) -> Self {
        crate::error::AppError::LogStore(e.to_string())
    }
}
