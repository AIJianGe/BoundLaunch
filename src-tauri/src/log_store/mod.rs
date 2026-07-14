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

pub use repository::{LogEntry, LogLevel, LogRepository, TaskRepository, TaskHistoryRecord};

use crate::paths::env_paths;
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
    ///
    /// **v0.0.2.1**：DB 路径从 `env_paths::resolve().database_path` 取，
    /// 固定在 `<env_root>/.boundlaunch/launcher.db`。
    pub async fn new(db_path: Option<PathBuf>) -> Result<Self, LogStoreError> {
        let path = match db_path {
            Some(p) => p,
            None => env_paths::resolve()
                .map(|p| p.database_path.clone())
                .map_err(|e| LogStoreError::InitFailed(format!("env_paths resolve failed: {}", e)))?,
        };

        // 确保父目录存在
        if let Some(parent) = path.parent() {
            env_paths::ensure_dir(parent)
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

    /// v3.10 业务错误快捷写入（覆盖 toast.error / toast.warn 路径）
    ///
    /// **目的**：让前端 `useToast` 自动调用的 20+ 个 toast.error / toast.warn
    /// 能 0 业务代码改动地入 LogStore，避免"弹窗消失=日志丢失"。
    ///
    /// **写策略**：
    /// - 异步 spawn 写（不阻塞调用方）
    /// - 写失败仅 warn（不污染主流程）
    /// - 自动加 `ui:` 前缀，与 ComfyUI / 任务来源区分
    ///
    /// **入参约束**：
    /// - `level` 必须是 warn / error（其他级别会降级为 info）
    /// - `source` 长度 ≤ 64（过长会被截断）
    /// - `message` 长度 ≤ 4096（过长会被截断 + `…` 省略号）
    pub fn log_business_error(
        &self,
        level: crate::log_store::repository::LogLevel,
        source: &str,
        message: &str,
    ) {
        use crate::log_store::repository::LogEntry;

        // 长度保护：避免前端错误地把整个 Exception 链塞进来
        let source = if source.len() > 64 {
            format!("{}…", &source[..63])
        } else {
            source.to_string()
        };
        let message = if message.len() > 4096 {
            format!("{}…", &message[..4095])
        } else {
            message.to_string()
        };

        // 降级：非 warn/error 一律记为 info
        let level = match level {
            crate::log_store::repository::LogLevel::Warn
            | crate::log_store::repository::LogLevel::Error => level,
            _ => crate::log_store::repository::LogLevel::Info,
        };

        let entry = LogEntry {
            timestamp: chrono::Utc::now(),
            level,
            source: format!("ui:{}", source),
            message,
        };

        // 异步写：spawn 出来不阻塞调用方
        let pool = self.pool.clone();
        tokio::spawn(async move {
            if let Err(e) = (LogRepository { pool: &pool }).append(entry).await {
                tracing::warn!(error = %e, "log_business_error persist failed");
            }
        });
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
