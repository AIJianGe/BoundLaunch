//! EnvCache - 30 秒 TTL 缓存（F32: stale 模式）
//!
//! 详见 `PR/03-模块设计/07-EnvironmentInspector.md §8 缓存策略 §14 F32 探查类异步化`
//!
//! 失效条件：
//! - TTL 30 秒过期
//! - venv 目录 mtime 变化（装包后）
//! - 事件总线订阅 TorchInstalled / VenvRebuilt / CoreVersionSwitched 主动失效
//!
//! F32 stale 模式：
//! - `invalidate()` 不删值，仅标记 `stale=true`，保留旧值供快速返回
//! - `get_stale()` 不检查新鲜度，仅返回缓存（用于 invoke 立即返回）
//! - `is_fresh()` 增加 `!stale` 条件
//! - `needs_refresh()` = `!is_fresh()`
//! - `clear()` 完全清空（仅 AppExiting 时调用）

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
    /// F32 新增：是否过期（invalidate 不删值，仅标记）
    ///
    /// - `false`：缓存新鲜（刚 set 或未触发 invalidate）
    /// - `true`：被 invalidate 标记过期，但旧值仍保留供 stale 读取
    stale: bool,
}

impl EnvCache {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
        }
    }

    /// 是否新鲜（命中条件：未过期 + venv mtime 未变 + stale=false）
    ///
    /// F32 改造：增加 `!inner.stale` 条件
    pub fn is_fresh(&self, venv_path: &Path) -> bool {
        let guard = self.inner.read();
        match &*guard {
            Some(inner) => {
                let ttl_ok = inner.cached_at.elapsed() < CACHE_TTL;
                let current_mtime = venv_path.metadata().and_then(|m| m.modified()).ok();
                let mtime_ok = inner.venv_mtime == current_mtime;
                ttl_ok && mtime_ok && !inner.stale
            }
            None => false,
        }
    }

    /// F32 新增：是否需要后台刷新（缓存不存在 或 不新鲜）
    pub fn needs_refresh(&self, venv_path: &Path) -> bool {
        !self.is_fresh(venv_path)
    }

    /// 读取 stale 值（不检查新鲜度，仅返回缓存）
    ///
    /// F32 新增：用于「先返回旧值，后台刷新」场景。
    /// 与 `get()` 等价（`get()` 本就不检查新鲜度），命名为 `get_stale` 强调语义。
    pub fn get_stale(&self) -> Option<EnvInfo> {
        self.inner.read().as_ref().map(|i| i.info.clone())
    }

    /// 读取缓存（不检查新鲜度，调用方应先调 is_fresh）
    ///
    /// 保留向后兼容：等价于 `get_stale()`
    pub fn get(&self) -> Option<EnvInfo> {
        self.get_stale()
    }

    /// 写入缓存（同时清除 stale 标记）
    pub fn set(&self, info: EnvInfo, venv_path: &Path) {
        let venv_mtime = venv_path.metadata().and_then(|m| m.modified()).ok();
        let inner = EnvCacheInner {
            info,
            cached_at: Instant::now(),
            venv_mtime,
            stale: false,
        };
        *self.inner.write() = Some(inner);
    }

    /// 主动失效（事件总线订阅后调用）
    ///
    /// F32 改造：不删值，仅标记 `stale=true`，保留旧值供 `get_stale()` 快速返回。
    /// 这样 invoke 在缓存失效后仍能立即返回旧值，前端不显示 loading。
    pub fn invalidate(&self) {
        let mut guard = self.inner.write();
        if let Some(inner) = guard.as_mut() {
            inner.stale = true;
            tracing::debug!("env cache marked stale (value retained)");
        } else {
            // 缓存本就为空，无需标记
            tracing::debug!("env cache invalidate noop (cache already empty)");
        }
    }

    /// F32 新增：完全清空缓存
    ///
    /// 仅在 AppExiting 时调用（替代旧 `invalidate()` 的删值语义）。
    /// 正常业务场景应使用 `invalidate()`（保留 stale 值）。
    pub fn clear(&self) {
        *self.inner.write() = None;
        tracing::debug!("env cache cleared");
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
        assert!(cache.get_stale().is_none());
    }

    #[test]
    fn test_cache_hit_after_set() {
        let cache = EnvCache::new();
        let tmp = tempfile::tempdir().unwrap();
        // 注意：is_fresh 会比较 venv_path mtime，set 时记录的 mtime 与 is_fresh 时读取一致
        cache.set(make_info(), tmp.path());
        assert!(cache.is_fresh(tmp.path()));
        assert!(cache.get().is_some());
        assert!(cache.get_stale().is_some());
        assert!(!cache.needs_refresh(tmp.path()));
    }

    #[test]
    fn test_invalidate_clears() {
        let cache = EnvCache::new();
        let tmp = tempfile::tempdir().unwrap();
        cache.set(make_info(), tmp.path());
        assert!(cache.is_fresh(tmp.path()));

        cache.invalidate();
        assert!(!cache.is_fresh(tmp.path()));
        // F32: invalidate 不删值，get_stale 仍返回旧值
        assert!(cache.get_stale().is_some());
        assert!(cache.get().is_some());
        assert!(cache.needs_refresh(tmp.path()));
    }

    #[test]
    fn test_clear_empties_cache() {
        let cache = EnvCache::new();
        let tmp = tempfile::tempdir().unwrap();
        cache.set(make_info(), tmp.path());
        assert!(cache.is_fresh(tmp.path()));

        cache.clear();
        assert!(!cache.is_fresh(tmp.path()));
        assert!(cache.get().is_none());
        assert!(cache.get_stale().is_none());
    }

    #[test]
    fn test_invalidate_then_set_clears_stale() {
        let cache = EnvCache::new();
        let tmp = tempfile::tempdir().unwrap();
        cache.set(make_info(), tmp.path());
        cache.invalidate();
        assert!(!cache.is_fresh(tmp.path()));
        assert!(cache.get_stale().is_some()); // 旧值仍在

        // set 后 stale 标记应清除
        cache.set(make_info(), tmp.path());
        assert!(cache.is_fresh(tmp.path()));
        assert!(!cache.needs_refresh(tmp.path()));
    }
}
