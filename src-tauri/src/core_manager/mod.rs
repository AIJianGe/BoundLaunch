//! CoreManager 模块
//!
//! 设计模式：
//! - **Adapter**：git2 C 库 → Rust 异步接口（spawn_blocking 包裹）
//! - **Cache-Aside**：tags 列表 5 分钟缓存 + 持久化到 LogStore
//! - **State**：working tree 状态判断（clean / dirty）
//!
//! 详见 `PR/03-模块设计/03-CoreManager.md`

pub mod compat;
pub mod git_ops;
pub mod git_ops_async;
pub mod models;
pub mod paths;
pub mod repo_switcher;
pub mod semver;
pub mod switcher;
pub mod tags;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use crate::config::ConfigService;
use crate::error::CoreError;
use crate::event_bus::{EventBus, SystemEvent};
use crate::log_store::LogStoreService;

use models::{CheckoutResult, ClassifiedTags, CoreStatus, SwitchPrerequisites, TagInfo};

/// Tags 缓存 TTL
const TAGS_CACHE_TTL: Duration = Duration::from_secs(300); // 5 分钟

/// 内部 tags 缓存
struct TagsCache {
    tags: Vec<TagInfo>,
    /// None 表示未缓存（启动状态 / 已失效），避免 `Instant::now() - duration` 在
    /// Windows QPC 计数器较小时下溢出 panic（详见 run.bat 崩溃报告）
    cached_at: Option<Instant>,
}

impl TagsCache {
    fn new() -> Self {
        Self {
            tags: Vec::new(),
            cached_at: None, // 启动即过期
        }
    }

    fn is_fresh(&self) -> bool {
        match self.cached_at {
            Some(t) => t.elapsed() < TAGS_CACHE_TTL,
            None => false,
        }
    }
}

/// CoreManager 服务
///
/// **路径热加载**：所有 git 操作通过 `current_repo_path()` 从 `ConfigService`
/// 读取最新的 `comfyui_root`，实现"修改 config 后无需重启立即生效"。
///
/// 详见 `PR/03-模块设计/03-CoreManager.md §4 服务接口`。
pub struct CoreManagerService {
    /// Config 共享引用（路径热加载的单一信息源）
    config: Arc<ConfigService>,
    event_bus: EventBus,
    log_store: Arc<LogStoreService>,
    /// 内存 tags 缓存
    tags_cache: RwLock<TagsCache>,
    /// 串行化所有 git 操作
    repo_lock: tokio::sync::Mutex<()>,
}

impl CoreManagerService {
    /// 构造 CoreManagerService
    ///
    /// # 参数
    /// - `config`：共享 ConfigService（提供 `paths.comfyui_root` 热读取）
    /// - `event_bus`：事件总线（用于 emit `CoreVersionSwitched`）
    /// - `log_store`：日志服务（用于持久化 tags 缓存）
    pub fn new(
        config: Arc<ConfigService>,
        event_bus: EventBus,
        log_store: Arc<LogStoreService>,
    ) -> Self {
        Self {
            config,
            event_bus,
            log_store,
            tags_cache: RwLock::new(TagsCache::new()),
            repo_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// 读取当前 comfyui_root（每次调用读最新 config，无锁原子）
    fn current_repo_path(&self) -> PathBuf {
        self.config.get().paths.comfyui_root.clone()
    }

    /// 读取当前仓库 URL（F31 新增）
    ///
    /// 优先从 Config.paths.comfyui_repo_url 读取（用户自定义），
    /// None 时回退到常量 `COMFYUI_REPO_URL`（官方仓库）。
    pub fn current_repo_url(&self) -> String {
        let config = self.config.get();
        config
            .paths
            .comfyui_repo_url
            .clone()
            .unwrap_or_else(|| models::COMFYUI_REPO_URL.to_string())
    }

    /// 仓库是否已克隆
    pub async fn is_cloned(&self) -> bool {
        self.current_repo_path().join(".git").exists()
    }

    /// 仓库根路径（外部 API，仅暴露 &Path 借用）
    pub fn repo_path(&self) -> PathBuf {
        self.current_repo_path()
    }

    /// 克隆 ComfyUI 仓库
    ///
    /// 长任务，调用方应通过 TaskScheduler 提交（Phase 10 后）
    pub async fn clone_repo(&self, url: &str) -> Result<(), CoreError> {
        let _guard = self.repo_lock.lock().await;
        // 在持锁状态下再读一次路径（防止持锁期间 config 变更导致前后不一致）
        let repo_path = self.current_repo_path();
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
    ///
    /// **v1.8 改进**：clone 完成后**自动 checkout 到 latest stable tag**，
    /// 避免用户首次启动时停留在 master 分支（不是稳定版）。
    /// - 找 latest_stable 失败 → 回退到 master（不阻塞 onboarding）
    /// - checkout 失败 → 回退到 master + 记录 warn
    pub async fn ensure_cloned(&self) -> Result<(), CoreError> {
        if self.is_cloned().await {
            tracing::debug!("comfyui repo already cloned, skipping");
            return Ok(());
        }

        // 委托给 clone_repo，自动检测目录状态
        let url = self.current_repo_url();
        self.clone_repo(&url).await?;

        // clone 完成后，自动切到最新稳定版（onboarding 体验优化）
        // 失败不阻塞（用户可能想用 master / 网络问题拉不到 tags）
        match self.update_latest_stable().await {
            Ok(tag) => {
                tracing::info!(tag = %tag, "onboarding auto-switched to latest stable");
            }
            Err(e) => {
                tracing::warn!(error = %e, "onboarding auto-switch to latest_stable failed, staying on master");
            }
        }

        Ok(())
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
                    cache.cached_at = Some(Instant::now());
                    tracing::debug!(count = tags.len(), "loaded tags from persistent cache");
                    return Ok(tags);
                }
            }
        }

        // 实际 fetch
        let _guard = self.repo_lock.lock().await;
        let repo_path = self.current_repo_path();

        let repo = tokio::task::spawn_blocking(move || git_ops::open_repo(&repo_path))
            .await
            .map_err(|e| CoreError::GitError(e.to_string()))??;

        // fetch tags（网络操作，可能失败）
        let url = self.current_repo_url();
        let repo_for_fetch = tokio::task::spawn_blocking(move || {
            // repo 移动到 blocking 上下文
            let r = repo;
            git_ops::fetch_tags(&r, &url)
        })
        .await
        .map_err(|e| CoreError::GitError(e.to_string()))?;

        if let Err(e) = repo_for_fetch {
            tracing::warn!(error = %e, "fetch tags failed, using existing local tags");
        }

        // 重新打开列 tag（fetch 消耗了 repo）
        let repo_path = self.current_repo_path();
        let repo = tokio::task::spawn_blocking({
            let repo_path = repo_path;
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
            cache.cached_at = Some(Instant::now());
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

    /// 列出所有 tag 并按 SemVer 分类（v3.1 / F26 决策 7：NTab 双分类）
    ///
    /// 决策 10：本地优先 - 先用本地缓存，未命中再 fetch 远程。
    /// force_refresh = true 时强制刷新缓存。
    pub async fn list_classified_tags(
        &self,
        force_refresh: bool,
    ) -> Result<ClassifiedTags, CoreError> {
        let tags = self.list_tags(force_refresh).await?;
        Ok(tags::classify_tags(tags))
    }

    /// 检查切换版本的前置条件（v3.1 / F26 决策 5）
    ///
    /// 返回 SwitchPrerequisites，前端根据 can_switch 决定是否允许切换。
    /// 阻止条件：
    /// - ComfyUI 运行中
    /// - 工作区有未提交改动（脏状态）
    pub async fn check_switch_prerequisites(
        &self,
        comfyui_running: bool,
    ) -> Result<SwitchPrerequisites, CoreError> {
        // 读取当前 tag
        let current_tag = if self.is_cloned().await {
            self.current_version().await?.current_version
        } else {
            None
        };

        // 检查工作区
        let has_local_changes = if self.is_cloned().await {
            self.has_local_changes().await?
        } else {
            false
        };

        let mut blocks: Vec<String> = Vec::new();
        if comfyui_running {
            blocks.push("ComfyUI 正在运行".to_string());
        }
        if has_local_changes {
            blocks.push("工作区有未提交改动".to_string());
        }
        let block_reason = if blocks.is_empty() {
            None
        } else {
            Some(blocks.join("；"))
        };

        Ok(SwitchPrerequisites {
            can_switch: block_reason.is_none(),
            comfyui_running,
            has_local_changes,
            current_tag,
            block_reason,
        })
    }

    /// 当前仓库状态
    pub async fn current_version(&self) -> Result<CoreStatus, CoreError> {
        let _guard = self.repo_lock.lock().await;
        let repo_path = self.current_repo_path();

        let repo = tokio::task::spawn_blocking(move || git_ops::open_repo(&repo_path))
            .await
            .map_err(|e| CoreError::GitError(e.to_string()))??;

        let current_version = tokio::task::spawn_blocking(move || git_ops::current_tag(&repo))
            .await
            .map_err(|e| CoreError::GitError(e.to_string()))??;

        // 重新打开取 commit + status
        let repo_path = self.current_repo_path();
        let repo = tokio::task::spawn_blocking(move || git_ops::open_repo(&repo_path))
            .await
            .map_err(|e| CoreError::GitError(e.to_string()))??;

        let current_commit = tokio::task::spawn_blocking(move || git_ops::current_commit(&repo))
            .await
            .map_err(|e| CoreError::GitError(e.to_string()))??;

        let repo_path = self.current_repo_path();
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
        let repo_path = self.current_repo_path();
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
        let repo_path = self.current_repo_path();
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
        cache.cached_at = None; // 失效缓存（避免 Instant 下溢出 panic）
    }

    // ========================================================================
    // F31：仓库地址切换与备份恢复
    // ========================================================================

    /// 获取当前仓库 URL（脱敏后的，用于前端显示）
    pub fn get_repo_url_masked(&self) -> String {
        repo_switcher::mask_url_credentials(&self.current_repo_url())
    }

    /// 获取官方仓库 URL（常量）
    pub fn official_repo_url(&self) -> &'static str {
        models::COMFYUI_REPO_URL
    }

    /// 列出所有备份
    pub async fn list_backups(&self) -> Result<Vec<models::BackupInfo>, CoreError> {
        let repo_path = self.current_repo_path();
        let _guard = self.repo_lock.lock().await;
        tokio::task::spawn_blocking(move || repo_switcher::list_backups(&repo_path))
            .await
            .map_err(|e| CoreError::GitError(e.to_string()))?
    }

    /// 切换仓库地址
    ///
    /// 完整流程：备份 → 更新 Config → 克隆 → 迁移 → 重建链接 → 失效缓存
    /// 失败时回滚（恢复备份 + 恢复 Config）
    pub async fn switch_repo_url(
        &self,
        new_url: &str,
        migrate_custom_nodes: bool,
    ) -> Result<models::SwitchRepoResult, CoreError> {
        let _guard = self.repo_lock.lock().await;
        let repo_path = self.current_repo_path();
        let old_url = self.current_repo_url();
        let new_url = new_url.to_string();
        // 闭包内需要拥有 new_url，闭包外也需要更新 Config，因此克隆一份供闭包使用
        let new_url_for_closure = new_url.clone();
        let migrate = migrate_custom_nodes;

        // 执行切换（spawn_blocking 因为 clone 是同步操作）
        let result = tokio::task::spawn_blocking(move || {
            repo_switcher::switch_repo_url_sync(&repo_path, &old_url, &new_url_for_closure, migrate)
        })
        .await
        .map_err(|e| CoreError::GitError(format!("switch task join error: {}", e)))??;

        // 根据结果更新 Config
        match &result {
            models::SwitchRepoResult::Success { .. } => {
                // 更新 Config.paths.comfyui_repo_url
                self.config
                    .update(|cfg| {
                        cfg.paths.comfyui_repo_url = Some(new_url.clone());
                        Ok(())
                    })
                    .await
                    .map_err(|e| CoreError::GitError(format!("config update failed: {}", e)))?;

                // 重建 models 软链接
                let comfyui_root = self.current_repo_path();
                let models_path = self.config.get().paths.models_path.clone();
                if let Err(e) =
                    crate::core_manager::paths::ensure_models_link(&comfyui_root, models_path.as_deref())
                {
                    tracing::warn!(error = %e, "failed to rebuild models link after repo switch");
                }

                // 失效 tags 缓存
                self.invalidate_tags_cache();

                // 清空 LogStore tags 持久化缓存（v3.3 / F33 改用 invalidate 语义）
                let log_store = self.log_store.clone();
                tokio::spawn(async move {
                    if let Err(e) = log_store.logs().invalidate_tags_cache().await {
                        tracing::warn!(error = %e, "failed to clear tags persistent cache");
                    }
                });
            }
            models::SwitchRepoResult::RolledBack { .. } => {
                // 回滚时 Config 不需要更新（保持原 URL）
            }
        }

        Ok(result)
    }

    /// 恢复备份
    ///
    /// 1. 当前 ComfyUI 也备份
    /// 2. rename 备份 → comfyui_root
    /// 3. 更新 Config.paths.comfyui_repo_url = 备份的 URL
    /// 4. 重建 models 链接 + 失效缓存
    pub async fn restore_backup(
        &self,
        backup_name: &str,
    ) -> Result<models::SwitchRepoResult, CoreError> {
        let _guard = self.repo_lock.lock().await;
        let repo_path = self.current_repo_path();
        let backup_name = backup_name.to_string();
        let backup_name_for_result = backup_name.clone();

        let (repo_url, _masked) = tokio::task::spawn_blocking(move || {
            repo_switcher::restore_backup_sync(&repo_path, &backup_name)
        })
        .await
        .map_err(|e| CoreError::GitError(format!("restore task join error: {}", e)))??;

        // 更新 Config
        self.config
            .update(|cfg| {
                cfg.paths.comfyui_repo_url = Some(repo_url.clone());
                Ok(())
            })
            .await
            .map_err(|e| CoreError::GitError(format!("config update failed: {}", e)))?;

        // 重建 models 软链接
        let comfyui_root = self.current_repo_path();
        let models_path = self.config.get().paths.models_path.clone();
        if let Err(e) =
            crate::core_manager::paths::ensure_models_link(&comfyui_root, models_path.as_deref())
        {
            tracing::warn!(error = %e, "failed to rebuild models link after restore");
        }

        // 失效 tags 缓存（v3.3 / F33：内存缓存 + LogStore 持久化缓存都要清，
        // 与 switch_repo_url 保持一致；之前只清内存，启动后会用旧持久化缓存）
        self.invalidate_tags_cache();
        let log_store = self.log_store.clone();
        tokio::spawn(async move {
            if let Err(e) = log_store.logs().invalidate_tags_cache().await {
                tracing::warn!(error = %e, "failed to clear tags persistent cache after restore");
            }
        });

        Ok(models::SwitchRepoResult::Success {
            from_url: "backup".to_string(),
            to_url: repo_switcher::mask_url_credentials(&repo_url),
            backup_name: Some(backup_name_for_result),
            clone_elapsed_ms: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigService;
    use crate::event_bus::EventBus;
    use crate::log_store::LogStoreService;

    /// 构造测试用 CoreManagerService（路径热加载版）
    ///
    /// 与 `new(config, event_bus, log_store)` 一致，
    /// 但额外用 `config.update()` 把 `comfyui_root` 指向临时目录。
    async fn make_service(tmp: &tempfile::TempDir) -> CoreManagerService {
        let event_bus = EventBus::new();
        let log_store = std::sync::Arc::new(
            LogStoreService::new(None).await.expect("logstore init failed"),
        );
        let config = std::sync::Arc::new(ConfigService::new_for_test(event_bus.clone()));
        // 模拟"用户配置的 comfyui_root 指向临时目录"
        config
            .update(|cfg| {
                cfg.paths.comfyui_root = tmp.path().to_path_buf();
                Ok(())
            })
            .await
            .expect("set comfyui_root");
        CoreManagerService::new(config, event_bus, log_store)
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

    /// 路径热加载测试：改 config 后立即生效，无需重建 service
    #[tokio::test]
    async fn test_hot_reload_repo_path() {
        let tmp1 = tempfile::tempdir().unwrap();
        let tmp2 = tempfile::tempdir().unwrap();

        let event_bus = EventBus::new();
        let log_store = std::sync::Arc::new(
            LogStoreService::new(None).await.expect("logstore init failed"),
        );
        let config = std::sync::Arc::new(ConfigService::new_for_test(event_bus.clone()));
        config
            .update(|cfg| {
                cfg.paths.comfyui_root = tmp1.path().to_path_buf();
                Ok(())
            })
            .await
            .unwrap();
        let service = CoreManagerService::new(config.clone(), event_bus, log_store);

        // 初始路径
        assert_eq!(service.repo_path(), tmp1.path().to_path_buf());

        // 改 config → service 立即看到新路径（无需重建）
        config
            .update(|cfg| {
                cfg.paths.comfyui_root = tmp2.path().to_path_buf();
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(service.repo_path(), tmp2.path().to_path_buf());
    }
}
