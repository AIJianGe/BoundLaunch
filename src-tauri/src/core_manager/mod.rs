//! CoreManager 模块
//!
//! 设计模式：
//! - **Adapter**：git2 C 库 → Rust 异步接口（spawn_blocking 包裹）
//! - **Cache-Aside**：tags 列表 5 分钟缓存 + 持久化到 LogStore
//! - **State**：working tree 状态判断（clean / dirty）
//!
//! 详见 `PR/03-模块设计/03-CoreManager.md`

pub mod git_ops;
pub mod models;
pub mod tags;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use crate::error::CoreError;
use crate::event_bus::{EventBus, SystemEvent};
use crate::log_store::LogStoreService;

use models::{CheckoutResult, CoreStatus, TagInfo};

/// Tags 缓存 TTL
const TAGS_CACHE_TTL: Duration = Duration::from_secs(300); // 5 分钟

/// 内部 tags 缓存
struct TagsCache {
    tags: Vec<TagInfo>,
    cached_at: Instant,
}

impl TagsCache {
    fn new() -> Self {
        Self {
            tags: Vec::new(),
            cached_at: Instant::now() - TAGS_CACHE_TTL * 2, // 启动即过期
        }
    }

    fn is_fresh(&self) -> bool {
        self.cached_at.elapsed() < TAGS_CACHE_TTL
    }
}

/// CoreManager 服务
pub struct CoreManagerService {
    repo_path: PathBuf,
    event_bus: EventBus,
    log_store: std::sync::Arc<LogStoreService>,
    /// 内存 tags 缓存
    tags_cache: RwLock<TagsCache>,
    /// 串行化所有 git 操作
    repo_lock: tokio::sync::Mutex<()>,
}

impl CoreManagerService {
    pub fn new(
        repo_path: PathBuf,
        event_bus: EventBus,
        log_store: std::sync::Arc<LogStoreService>,
    ) -> Self {
        Self {
            repo_path,
            event_bus,
            log_store,
            tags_cache: RwLock::new(TagsCache::new()),
            repo_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// 仓库是否已克隆
    pub async fn is_cloned(&self) -> bool {
        self.repo_path.join(".git").exists()
    }

    /// 仓库根路径
    pub fn repo_path(&self) -> &Path {
        &self.repo_path
    }

    /// 克隆 ComfyUI 仓库
    ///
    /// 长任务，调用方应通过 TaskScheduler 提交（Phase 10 后）
    pub async fn clone_repo(&self, url: &str) -> Result<(), CoreError> {
        let _guard = self.repo_lock.lock().await;
        let repo_path = self.repo_path.clone();
        let url = url.to_string();

        tokio::task::spawn_blocking(move || git_ops::clone_repo(&repo_path, &url))
            .await
            .map_err(|e| CoreError::GitError(format!("clone task join error: {}", e)))??;

        Ok(())
    }

    /// 确保 ComfyUI 仓库已克隆
    ///
    /// 行为：
    /// - 已克隆（含 `.git`）→ 跳过，直接返回
    /// - 目录不存在 → 自动 clone 默认仓库（`COMFYUI_REPO_URL`）
    /// - 目录非空但非 git 仓库 → 返回 `NotEmptyDir` 错误（让前端提示用户）
    ///
    /// 用途：向导/启动页调用，无需用户手动选择 URL。
    pub async fn ensure_cloned(&self) -> Result<(), CoreError> {
        if self.is_cloned().await {
            tracing::debug!("comfyui repo already cloned, skipping");
            return Ok(());
        }

        // 委托给 clone_repo，自动检测目录状态
        let url = models::COMFYUI_REPO_URL.to_string();
        self.clone_repo(&url).await
    }

    /// 列出所有 tag（缓存命中 < 5ms，未命中 1-10s）
    pub async fn list_tags(&self, force_refresh: bool) -> Result<Vec<TagInfo>, CoreError> {
        // 缓存检查
        if !force_refresh {
            let cache = self.tags_cache.read();
            if cache.is_fresh() {
                return Ok(cache.tags.clone());
            }
        }

        // 尝试从 LogStore 加载缓存（启动时秒级展示）
        if !force_refresh {
            if let Ok(Some((json, _))) = self.log_store.logs().load_cached_tags().await {
                if let Ok(tags) = serde_json::from_str::<Vec<TagInfo>>(&json) {
                    let mut cache = self.tags_cache.write();
                    cache.tags = tags.clone();
                    cache.cached_at = Instant::now();
                    tracing::debug!(count = tags.len(), "loaded tags from persistent cache");
                    return Ok(tags);
                }
            }
        }

        // 实际 fetch
        let _guard = self.repo_lock.lock().await;
        let repo_path = self.repo_path.clone();

        let repo = tokio::task::spawn_blocking(move || git_ops::open_repo(&repo_path))
            .await
            .map_err(|e| CoreError::GitError(e.to_string()))??;

        // fetch tags（网络操作，可能失败）
        let url = models::COMFYUI_REPO_URL;
        let repo_for_fetch = tokio::task::spawn_blocking(move || {
            // repo 移动到 blocking 上下文
            let r = repo;
            git_ops::fetch_tags(&r, url)
        })
        .await
        .map_err(|e| CoreError::GitError(e.to_string()))?;

        if let Err(e) = repo_for_fetch {
            tracing::warn!(error = %e, "fetch tags failed, using existing local tags");
        }

        // 重新打开列 tag（fetch 消耗了 repo）
        let repo = tokio::task::spawn_blocking({
            let repo_path = self.repo_path.clone();
            move || git_ops::open_repo(&repo_path)
        })
        .await
        .map_err(|e| CoreError::GitError(e.to_string()))??;

        let tags = tokio::task::spawn_blocking(move || git_ops::list_tags(&repo))
            .await
            .map_err(|e| CoreError::GitError(e.to_string()))??;

        // 更新内存缓存
        {
            let mut cache = self.tags_cache.write();
            cache.tags = tags.clone();
            cache.cached_at = Instant::now();
        }

        // 持久化到 LogStore
        if let Ok(json) = serde_json::to_string(&tags) {
            if let Err(e) = self.log_store.logs().cache_tags(&json).await {
                tracing::warn!(error = %e, "failed to persist tags cache");
            }
        }

        Ok(tags)
    }

    /// 仅列出稳定版 tag
    pub async fn list_stable_tags(&self, force_refresh: bool) -> Result<Vec<TagInfo>, CoreError> {
        let tags = self.list_tags(force_refresh).await?;
        Ok(tags::filter_stable_tags(&tags))
    }

    /// 当前仓库状态
    pub async fn current_version(&self) -> Result<CoreStatus, CoreError> {
        let _guard = self.repo_lock.lock().await;
        let repo_path = self.repo_path.clone();

        let repo = tokio::task::spawn_blocking(move || git_ops::open_repo(&repo_path))
            .await
            .map_err(|e| CoreError::GitError(e.to_string()))??;

        let current_version = tokio::task::spawn_blocking(move || git_ops::current_tag(&repo))
            .await
            .map_err(|e| CoreError::GitError(e.to_string()))??;

        // 重新打开取 commit + status
        let repo_path = self.repo_path.clone();
        let repo = tokio::task::spawn_blocking(move || git_ops::open_repo(&repo_path))
            .await
            .map_err(|e| CoreError::GitError(e.to_string()))??;

        let current_commit = tokio::task::spawn_blocking(move || git_ops::current_commit(&repo))
            .await
            .map_err(|e| CoreError::GitError(e.to_string()))??;

        let repo_path = self.repo_path.clone();
        let has_local_changes =
            tokio::task::spawn_blocking(move || -> Result<bool, CoreError> {
                let repo = git_ops::open_repo(&repo_path)?;
                git_ops::has_local_changes(&repo)
            })
            .await
            .map_err(|e| CoreError::GitError(e.to_string()))??;

        // latest_stable 从缓存读
        let latest_stable = {
            let cache = self.tags_cache.read();
            if cache.is_fresh() {
                tags::latest_stable(&cache.tags)
            } else {
                None
            }
        };

        Ok(CoreStatus {
            current_version,
            current_commit,
            has_local_changes,
            latest_stable,
            is_clone_done: true,
        })
    }

    /// 切换到指定 tag
    ///
    /// 详见 `PR/03-模块设计/03-CoreManager.md §5 数据流` 切换版本
    pub async fn checkout(&self, tag: &str) -> Result<CheckoutResult, CoreError> {
        // TODO Phase 11+: 前置检查 ProcessLauncher.is_running()，运行中拒绝并返回 ComfyUIRunning
        // 当前暂跳过此检查（ProcessLauncher 未实现）

        let _guard = self.repo_lock.lock().await;
        let repo_path = self.repo_path.clone();
        let tag = tag.to_string();

        let repo = tokio::task::spawn_blocking(move || git_ops::open_repo(&repo_path))
            .await
            .map_err(|e| CoreError::GitError(e.to_string()))??;

        let result = tokio::task::spawn_blocking(move || {
            let mut repo = repo;
            git_ops::checkout_tag(&mut repo, &tag)
        })
            .await
            .map_err(|e| CoreError::GitError(e.to_string()))??;

        // 失效 tags 缓存（不需要，tags 不变）
        // 但需要触发事件总线通知订阅者（EnvironmentInspector / PythonEnvManager）
        let (from, to) = match &result {
            CheckoutResult::Switched { from, to } => (from.clone(), to.clone()),
            CheckoutResult::StashedAndSwitched { from, to, .. } => {
                (Some(from.clone()), to.clone())
            }
            CheckoutResult::AlreadyOnTag(_) => return Ok(result),
        };

        self.event_bus.emit(SystemEvent::CoreVersionSwitched { from, to });

        Ok(result)
    }

    /// 更新到最新稳定版
    pub async fn update_latest_stable(&self) -> Result<String, CoreError> {
        // 强制刷新 tags
        let tags = self.list_tags(true).await?;
        let latest = tags::latest_stable(&tags).ok_or_else(|| {
            CoreError::GitError("no stable tag found".to_string())
        })?;

        let status = self.current_version().await?;
        if status.current_version.as_deref() == Some(&latest) {
            return Ok(latest);
        }

        self.checkout(&latest).await?;
        Ok(latest)
    }

    /// 检查工作区是否有未提交改动
    pub async fn has_local_changes(&self) -> Result<bool, CoreError> {
        let _guard = self.repo_lock.lock().await;
        let repo_path = self.repo_path.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<bool, CoreError> {
            let repo = git_ops::open_repo(&repo_path)?;
            git_ops::has_local_changes(&repo)
        })
        .await
        .map_err(|e| CoreError::GitError(e.to_string()))??;
        Ok(result)
    }

    /// 失效 tags 缓存（事件总线触发或手动调用）
    pub fn invalidate_tags_cache(&self) {
        let mut cache = self.tags_cache.write();
        cache.cached_at = Instant::now() - TAGS_CACHE_TTL * 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_bus::EventBus;
    use crate::log_store::LogStoreService;

    async fn make_service(tmp: &tempfile::TempDir) -> CoreManagerService {
        let event_bus = EventBus::new();
        let log_store = std::sync::Arc::new(
            LogStoreService::new(None).await.expect("logstore init failed"),
        );
        CoreManagerService::new(tmp.path().to_path_buf(), event_bus, log_store)
    }

    #[tokio::test]
    async fn test_is_cloned_returns_false_for_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let service = make_service(&tmp).await;
        assert!(!service.is_cloned().await);
    }

    #[tokio::test]
    async fn test_current_version_on_uncloned_returns_not_cloned() {
        let tmp = tempfile::tempdir().unwrap();
        let service = make_service(&tmp).await;
        let result = service.current_version().await;
        assert!(matches!(result, Err(CoreError::NotCloned)));
    }

    #[tokio::test]
    async fn test_list_tags_uncloned_returns_not_cloned() {
        let tmp = tempfile::tempdir().unwrap();
        let service = make_service(&tmp).await;
        let result = service.list_tags(false).await;
        assert!(matches!(result, Err(CoreError::NotCloned)));
    }
}
