//! EnvCache - 30 秒 TTL 缓存
//!
//! 详见 `PR/03-模块设计/07-EnvironmentInspector.md §8 缓存策略`
//!
//! 失效条件：
//! - TTL 30 秒过期
//! - venv 目录 mtime 变化（装包后）
//! - 事件总线订阅 TorchInstalled / VenvRebuilt / CoreVersionSwitched 主动失效

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use parking_lot::RwLock;

use super::models::EnvInfo;

/// 缓存 TTL
const CACHE_TTL: Duration = Duration::from_secs(30);

/// 环境探查缓存（Arc 共享，可在事件订阅 task 中持有同一份）
#[derive(Clone)]
pub struct EnvCache {
    inner: Arc<RwLock<Option<EnvCacheInner>>>,
}

struct EnvCacheInner {
    info: EnvInfo,
    cached_at: Instant,
    /// venv 目录 mtime，用于检测装包
    venv_mtime: Option<SystemTime>,
}

impl EnvCache {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
        }
    }

    /// 是否新鲜（命中条件：未过期 + venv mtime 未变）
    pub fn is_fresh(&self, venv_path: &Path) -> bool {
        let guard = self.inner.read();
        match &*guard {
            Some(inner) => {
                let ttl_ok = inner.cached_at.elapsed() < CACHE_TTL;
                let current_mtime = venv_path.metadata().and_then(|m| m.modified()).ok();
                let mtime_ok = inner.venv_mtime == current_mtime;
                ttl_ok && mtime_ok
            }
            None => false,
        }
    }

    /// 读取缓存（不检查新鲜度，调用方应先调 is_fresh）
    pub fn get(&self) -> Option<EnvInfo> {
        self.inner.read().as_ref().map(|i| i.info.clone())
    }

    /// 写入缓存
    pub fn set(&self, info: EnvInfo, venv_path: &Path) {
        let venv_mtime = venv_path.metadata().and_then(|m| m.modified()).ok();
        let inner = EnvCacheInner {
            info,
            cached_at: Instant::now(),
            venv_mtime,
        };
        *self.inner.write() = Some(inner);
    }

    /// 主动失效（事件总线订阅后调用）
    pub fn invalidate(&self) {
        *self.inner.write() = None;
        tracing::debug!("env cache invalidated");
    }
}

impl Default for EnvCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use crate::env_inspector::models::{EnvInfo, TorchInfo, GpuInfo};

    fn make_info() -> EnvInfo {
        EnvInfo {
            torch: TorchInfo::not_installed(),
            dependencies: vec![],
            gpu: GpuInfo::Unknown,
            comfyui_version: None,
            running_args: None,
            inspected_at: Utc::now(),
        }
    }

    #[test]
    fn test_cache_miss_when_empty() {
        let cache = EnvCache::new();
        assert!(!cache.is_fresh(Path::new("/nonexistent")));
        assert!(cache.get().is_none());
    }

    #[test]
    fn test_cache_hit_after_set() {
        let cache = EnvCache::new();
        let tmp = tempfile::tempdir().unwrap();
        // 注意：is_fresh 会比较 venv_path mtime，set 时记录的 mtime 与 is_fresh 时读取一致
        cache.set(make_info(), tmp.path());
        assert!(cache.is_fresh(tmp.path()));
        assert!(cache.get().is_some());
    }

    #[test]
    fn test_invalidate_clears() {
        let cache = EnvCache::new();
        let tmp = tempfile::tempdir().unwrap();
        cache.set(make_info(), tmp.path());
        assert!(cache.is_fresh(tmp.path()));

        cache.invalidate();
        assert!(!cache.is_fresh(tmp.path()));
        assert!(cache.get().is_none());
    }
}
