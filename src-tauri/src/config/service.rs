//! ConfigService - 配置服务
//!
//! 设计模式：
//! - Repository：load/save 抽象持久化
//! - Builder：update 闭包修改
//! - arc-swap 无锁读
//!
//! 详见 `PR/03-模块设计/01-Config.md`

use crate::common::paths;
use crate::error::{AppError, ConfigError};
use crate::event_bus::{EventBus, SystemEvent};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use super::{migrations, Config, SharedConfig, CURRENT_SCHEMA_VERSION};

/// 配置服务
///
/// 通过 Arc<ConfigService> 共享给各模块
/// 内部用 ArcSwap<Config> 实现无锁读
pub struct ConfigService {
    config: Arc<SharedConfig>,
    config_path: PathBuf,
    event_bus: EventBus,
}

impl ConfigService {
    /// 加载配置文件
    ///
    /// 文件不存在时自动创建默认配置
    /// TOML 解析失败时备份原文件 + 创建默认配置
    pub async fn load(path: PathBuf, event_bus: EventBus) -> Result<Self, ConfigError> {
        // 确保父目录存在
        if let Some(parent) = path.parent() {
            paths::ensure_dir(parent).await.map_err(io_err)?;
        }

        let config = if path.exists() {
            let content = tokio::fs::read_to_string(&path).await.map_err(io_err)?;
            match toml::from_str::<Config>(&content) {
                Ok(mut cfg) => {
                    // 自动迁移
                    if cfg.schema_version < CURRENT_SCHEMA_VERSION {
                        let from = cfg.schema_version;
                        migrations::migrate(&mut cfg, from, CURRENT_SCHEMA_VERSION)?;
                        // 迁移后立即保存
                        save_to_disk(&path, &cfg).await?;
                    }
                    cfg
                }
                Err(e) => {
                    // 解析失败：备份 + 创建默认
                    let backup = path.with_extension(format!("toml.corrupt-{}", chrono::Utc::now().timestamp()));
                    tracing::warn!(error = %e, ?backup, "config parse failed, backing up");
                    let _ = tokio::fs::rename(&path, &backup).await;
                    let cfg = Config::default();
                    save_to_disk(&path, &cfg).await?;
                    cfg
                }
            }
        } else {
            // 文件不存在：创建默认
            let cfg = Config::default();
            save_to_disk(&path, &cfg).await?;
            cfg
        };

        Ok(Self {
            config: Arc::new(SharedConfig::from_pointee(config)),
            config_path: path,
            event_bus,
        })
    }

    /// 用默认 Config 创建（仅用于测试）
    pub fn new_for_test(event_bus: EventBus) -> Self {
        Self {
            config: Arc::new(SharedConfig::from_pointee(Config::default())),
            config_path: PathBuf::new(),
            event_bus,
        }
    }

    /// 无锁读当前 Config
    ///
    /// 返回 Arc<Config>，调用方持有期间不会看到更新
    pub fn get(&self) -> arc_swap::Guard<Arc<Config>> {
        self.config.load()
    }

    /// 获取配置文件路径
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// 更新配置
    ///
    /// 闭包 f 在新的 Config 副本上执行，验证后原子交换
    /// 禁止在闭包内做 IO 或调用其他模块（防死锁）
    /// 闭包可返回 Result 表达解析失败
    pub async fn update<F>(&self, f: F) -> Result<(), AppError>
    where
        F: FnOnce(&mut Config) -> Result<(), ConfigError>,
    {
        // 加载当前副本
        let mut new_cfg: Config = (**self.config.load()).clone();
        f(&mut new_cfg)?;

        // 验证字段合法性
        validate(&new_cfg)?;

        // 持久化（先写盘，成功后再交换内存）
        if !self.config_path.as_os_str().is_empty() {
            save_to_disk(&self.config_path, &new_cfg).await?;
        }

        // 原子交换
        let section = config_section_name(&new_cfg);
        self.config.store(Arc::new(new_cfg));

        // 通知订阅者
        self.event_bus.emit(SystemEvent::ConfigChanged { section });

        Ok(())
    }

    /// 重置为默认配置
    pub async fn reset(&self) -> Result<(), AppError> {
        let cfg = Config::default();
        if !self.config_path.as_os_str().is_empty() {
            save_to_disk(&self.config_path, &cfg).await?;
        }
        self.config.store(Arc::new(cfg));
        self.event_bus.emit(SystemEvent::ConfigChanged {
            section: "*".to_string(),
        });
        Ok(())
    }
}

/// 内部：保存到磁盘
async fn save_to_disk(path: &Path, config: &Config) -> Result<(), ConfigError> {
    let content = toml::to_string_pretty(config).map_err(|e| ConfigError::SerializeError(e.to_string()))?;
    super::atomic_write::atomic_write(path, &content)
        .await
        .map_err(|e| ConfigError::IoError(e.to_string()))
}

/// 内部：字段验证
fn validate(cfg: &Config) -> Result<(), AppError> {
    if cfg.launch.listen_port == 0 {
        return Err(ConfigError::InvalidValue {
            field: "launch.listen_port".into(),
            value: "0".into(),
        }.into());
    }
    if cfg.paths.python_version.is_empty() {
        return Err(ConfigError::InvalidValue {
            field: "paths.python_version".into(),
            value: "(empty)".into(),
        }.into());
    }
    Ok(())
}

/// 内部：识别这次 update 影响的主要 section（用于事件订阅者过滤）
fn config_section_name(_cfg: &Config) -> String {
    // 简化实现：始终返回 "*"（全部）
    // 后续可对比 old/new 找出实际变化的 section
    "*".to_string()
}

fn io_err(e: std::io::Error) -> ConfigError {
    ConfigError::IoError(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_event_bus() -> EventBus {
        EventBus::new()
    }

    #[tokio::test]
    async fn test_load_default_when_absent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let svc = ConfigService::load(path.clone(), test_event_bus()).await.unwrap();

        assert_eq!(svc.config_path(), path);
        // 文件应被创建
        assert!(path.exists());
        // 加载的应该是默认值
        assert_eq!(svc.get().launch.listen_port, 8188);
    }

    #[tokio::test]
    async fn test_save_load_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let svc = ConfigService::load(path.clone(), test_event_bus()).await.unwrap();
        svc.update(|cfg| {
            cfg.launch.listen_port = 9999;
            cfg.torch.cuda_version = CudaVersion::Cu124;
            Ok(())
        }).await.unwrap();

        // 重新加载验证
        let svc2 = ConfigService::load(path, test_event_bus()).await.unwrap();
        assert_eq!(svc2.get().launch.listen_port, 9999);
        assert_eq!(svc2.get().torch.cuda_version, CudaVersion::Cu124);
    }

    #[tokio::test]
    async fn test_invalid_value_rollback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let svc = ConfigService::load(path.clone(), test_event_bus()).await.unwrap();

        let original_port = svc.get().launch.listen_port;
        let result = svc.update(|cfg| {
            cfg.launch.listen_port = 0;  // 非法值
            Ok(())
        }).await;

        assert!(result.is_err());
        // 内存 Config 不应被修改
        assert_eq!(svc.get().launch.listen_port, original_port);
    }

    #[tokio::test]
    async fn test_reset() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let svc = ConfigService::load(path, test_event_bus()).await.unwrap();

        svc.update(|cfg| { cfg.launch.listen_port = 9999; Ok(()) }).await.unwrap();
        assert_eq!(svc.get().launch.listen_port, 9999);

        svc.reset().await.unwrap();
        assert_eq!(svc.get().launch.listen_port, 8188);
    }

    #[tokio::test]
    async fn test_corrupt_file_backup() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        tokio::fs::write(&path, "invalid toml [[[").await.unwrap();

        let svc = ConfigService::load(path.clone(), test_event_bus()).await.unwrap();
        // 应该用默认值
        assert_eq!(svc.get().launch.listen_port, 8188);
        // 应该有备份文件
        let mut entries = Vec::new();
        let mut dir_iter = tokio::fs::read_dir(dir.path()).await.unwrap();
        while let Some(e) = dir_iter.next_entry().await.unwrap() {
            entries.push(e);
        }
        let has_backup = entries.iter().any(|e| {
            e.file_name().to_string_lossy().contains("corrupt")
        });
        assert!(has_backup, "should have backup file");
    }

    use super::super::CudaVersion;
}
