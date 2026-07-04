//! PluginManager 模块
//!
//! 详见 `PR/03-模块设计/04-PluginManager.md`
//!
//! ## 职责
//! - 扫描 custom_nodes 目录，列出已装插件
//! - 通过 git URL 安装插件（仅 https://）
//! - 更新插件（git pull）
//! - 卸载插件（移到回收站，可恢复）
//! - 启用/禁用插件（ComfyUI 约定：改名 `.disabled`）
//! - 安装插件的 requirements.txt
//!
//! ## 设计模式
//! - **Repository**：registry 模块封装 custom_nodes 目录扫描
//! - **Adapter**：git_ops 封装 libgit2；trash 封装文件系统
//! - **State**：插件状态机（未安装/已启用/已禁用/已卸载）
//! - **Cache-Aside**：列表 30s TTL，install/uninstall/toggle 后主动失效
//! - **Lock Striping**：plugin_locks DashMap 实现插件级互斥

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use parking_lot::RwLock;
use tokio::sync::Mutex;

use crate::event_bus::{EventBus, SystemEvent};

use self::url_util::sanitize_url_for_log;

pub mod git_ops;
pub mod models;
pub mod registry;
pub mod trash;
pub mod url_util;

pub use models::{
    PluginError, PluginInfo, PluginListResult, PluginProgress, PluginUpdateInfo, UninstallResult,
    UpdateResult,
};
pub use url_util::{derive_plugin_name, validate_git_url};

/// 列表缓存 30 秒 TTL
const LIST_CACHE_TTL: Duration = Duration::from_secs(30);

/// 列表缓存
#[derive(Clone)]
struct ListCache {
    result: PluginListResult,
    fetched_at: Instant,
}

impl ListCache {
    fn is_fresh(&self) -> bool {
        self.fetched_at.elapsed() < LIST_CACHE_TTL
    }

    fn has_plugin(&self, name: &str) -> bool {
        self.result
            .plugins
            .iter()
            .any(|p| p.name == name || p.dir_name == name)
    }
}

/// 插件管理服务
///
/// 设计模式：
/// - **单例**：通过 AppState 全局共享
/// - **DashMap**：plugin_locks 提供插件级互斥（不同插件可并发操作）
/// - **RwLock**：list_cache 多读单写
pub struct PluginManagerService {
    custom_nodes_path: PathBuf,
    venv_path: PathBuf,
    event_bus: EventBus,
    /// 插件级互斥锁（按插件名分桶）
    plugin_locks: DashMap<String, Arc<Mutex<()>>>,
    /// 列表缓存
    list_cache: RwLock<Option<ListCache>>,
}

impl PluginManagerService {
    pub fn new(custom_nodes_path: PathBuf, venv_path: PathBuf, event_bus: EventBus) -> Self {
        Self {
            custom_nodes_path,
            venv_path,
            event_bus,
            plugin_locks: DashMap::new(),
            list_cache: RwLock::new(None),
        }
    }

    /// 列出所有插件（30s 缓存）
    ///
    /// 缓存命中 → 直接返回
    /// 缓存未命中 → spawn_blocking(scan_plugins) → 写入缓存
    pub async fn list_plugins(
        &self,
        force_refresh: bool,
    ) -> Result<PluginListResult, PluginError> {
        // 1. 缓存检查
        if !force_refresh {
            let cache = self.list_cache.read();
            if let Some(c) = cache.as_ref() {
                if c.is_fresh() {
                    return Ok(c.result.clone());
                }
            }
        }

        // 2. spawn_blocking 扫描
        let custom_nodes = self.custom_nodes_path.clone();
        let plugins = tokio::task::spawn_blocking(move || registry::scan_plugins(&custom_nodes))
            .await
            .map_err(|e| PluginError::CloneFailed {
                stderr: format!("scan task panicked: {}", e),
            })??;

        let result = PluginListResult {
            plugins,
            fetched_at: chrono::Utc::now(),
        };

        // 3. 写入缓存
        // 注意：写锁必须在调用 cleanup_plugin_locks 之前释放，
        // 否则 cleanup_plugin_locks 内部取读锁会与当前持有的写锁死锁
        // （parking_lot::RwLock 不可重入，同线程持写锁时取读锁会永久阻塞）
        {
            let mut cache = self.list_cache.write();
            *cache = Some(ListCache {
                result: result.clone(),
                fetched_at: Instant::now(),
            });
        }

        // 4. 清理 plugin_locks 中已不存在的插件条目
        self.cleanup_plugin_locks();

        Ok(result)
    }

    /// 安装插件（git clone）
    ///
    /// 流程：
    /// 1. 校验 URL（仅 https://）
    /// 2. derive_plugin_name → 检查是否已存在
    /// 3. with_plugin_lock
    /// 4. spawn_blocking(git2 clone + 流式进度)
    /// 5. 读 __init__.py / pyproject.toml 取描述
    /// 6. 检查 requirements.txt → install_requirements
    /// 7. invalidate_list_cache + emit(PluginListChanged)
    pub async fn install<F>(
        &self,
        url: &str,
        progress: F,
    ) -> Result<PluginInfo, PluginError>
    where
        F: Fn(PluginProgress) + Send + 'static,
    {
        // 1. URL 校验
        validate_git_url(url)?;
        let safe_url = sanitize_url_for_log(url);
        let plugin_name = derive_plugin_name(url);
        tracing::info!(url = %safe_url, name = %plugin_name, "installing plugin");

        // 2. 检查是否已存在
        {
            let cache = self.list_cache.read();
            if let Some(c) = cache.as_ref() {
                if c.has_plugin(&plugin_name) {
                    return Err(PluginError::AlreadyExists(plugin_name));
                }
            }
        }
        // 即使缓存未命中也检查文件系统
        if registry::plugin_dir_path(&self.custom_nodes_path, &plugin_name).is_some() {
            return Err(PluginError::AlreadyExists(plugin_name));
        }

        // 3. 持插件锁
        let lock = self.get_plugin_lock(&plugin_name);
        let _guard = lock.lock().await;

        // 4. spawn_blocking(git2 clone)
        let target_dir = self.custom_nodes_path.join(&plugin_name);
        let url_clone = url.to_string();
        // 通知前端开始克隆（粒度粗：开始/完成/失败，详细进度需 mpsc 方案，本期简化）
        progress(PluginProgress::Cloning { percent: 0 });

        let target_dir_clone = target_dir.clone();
        let repo_result = tokio::task::spawn_blocking(move || {
            git_ops::clone_plugin_repo(&url_clone, &target_dir_clone)
        })
        .await
        .map_err(|e| PluginError::CloneFailed {
            stderr: format!("clone task panicked: {}", e),
        })?;

        // clone 失败时清理半成品并通知前端
        if let Err(e) = repo_result {
            tracing::error!(error = ?e, ?target_dir, "clone failed, cleaning up partial");
            let _ = std::fs::remove_dir_all(&target_dir);
            progress(PluginProgress::Failed {
                error: e.to_string(),
            });
            return Err(e);
        }
        // clone 成功后 Repository 实例不需要保留（info 在后续 spawn_blocking 中重新打开）

        // 5. 读 git 信息 + 描述
        let info_result = tokio::task::spawn_blocking({
            let custom_nodes = self.custom_nodes_path.clone();
            let plugin_name = plugin_name.clone();
            move || -> Result<PluginInfo, PluginError> {
                let path = registry::plugin_dir_path(&custom_nodes, &plugin_name)
                    .ok_or_else(|| PluginError::NotFound(plugin_name.clone()))?;
                let (commit, branch, git_url, has_local_changes) = match git2::Repository::open(&path) {
                    Ok(repo) => {
                        let commit = git_ops::current_commit(&repo).unwrap_or_default();
                        let br = git_ops::current_branch(&repo).unwrap_or(None);
                        let url = git_ops::remote_url(&repo);
                        let dirty = git_ops::has_local_changes(&repo).unwrap_or(false);
                        (commit, br, url, dirty)
                    }
                    Err(_) => (String::new(), None, None, false),
                };
                // 读描述
                let description = read_description_safe(&path);
                let requirements_installed = !path.join("requirements.txt").exists();

                Ok(PluginInfo {
                    name: plugin_name.clone(),
                    dir_name: plugin_name,
                    enabled: true,
                    git_url,
                    current_commit: commit,
                    current_branch: branch,
                    has_updates: None,
                    has_local_changes,
                    installed_at: Some(chrono::Utc::now()),
                    description,
                    requirements_installed,
                })
            }
        })
        .await
        .map_err(|e| PluginError::CloneFailed {
            stderr: format!("info task panicked: {}", e),
        })??;

        // 6. install_requirements（如果 requirements.txt 存在）
        if !info_result.requirements_installed {
            // 不阻塞 install 成功，仅 warn；前端通过 requirements_installed 字段判断
            if let Err(e) = self.install_requirements(&info_result.name).await {
                tracing::warn!(name = %info_result.name, error = %e, "requirements install failed");
            }
        }

        progress(PluginProgress::Done);

        // 7. 失效缓存 + emit
        self.invalidate_list_cache();
        self.event_bus.emit(SystemEvent::PluginListChanged);

        Ok(info_result)
    }

    /// 更新插件（git pull）
    pub async fn update(&self, name: &str) -> Result<UpdateResult, PluginError> {
        let lock = self.get_plugin_lock(name);
        let _guard = lock.lock().await;

        let plugin_path = registry::plugin_dir_path(&self.custom_nodes_path, name)
            .ok_or_else(|| PluginError::NotFound(name.to_string()))?;

        let (old_commit, new_commit) = tokio::task::spawn_blocking(move || -> Result<(String, String), PluginError> {
            let repo = git2::Repository::open(&plugin_path)?;
            git_ops::pull_repo(&repo)
        })
        .await
        .map_err(|e| PluginError::PullFailed {
            stderr: format!("pull task panicked: {}", e),
        })??;

        self.invalidate_list_cache();

        if old_commit == new_commit {
            Ok(UpdateResult::AlreadyUpToDate)
        } else {
            tracing::info!(name, from = %old_commit, to = %new_commit, "plugin updated");
            Ok(UpdateResult::Updated {
                from: old_commit,
                to: new_commit,
            })
        }
    }

    /// 卸载插件（移到回收站）
    pub async fn uninstall(&self, name: &str) -> Result<UninstallResult, PluginError> {
        let lock = self.get_plugin_lock(name);
        let _guard = lock.lock().await;

        let plugin_path = registry::plugin_dir_path(&self.custom_nodes_path, name)
            .ok_or_else(|| PluginError::NotFound(name.to_string()))?;

        let custom_nodes = self.custom_nodes_path.clone();
        let result = tokio::task::spawn_blocking(move || {
            trash::move_to_trash(&plugin_path, &custom_nodes)
        })
        .await
        .map_err(|e| PluginError::IoError(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("uninstall task panicked: {}", e),
        )))??;

        self.invalidate_list_cache();
        self.event_bus.emit(SystemEvent::PluginListChanged);

        tracing::info!(name, moved_to = ?result.moved_to, "plugin uninstalled");
        Ok(result)
    }

    /// 启停插件
    pub async fn toggle(&self, name: &str, enabled: bool) -> Result<(), PluginError> {
        let lock = self.get_plugin_lock(name);
        let _guard = lock.lock().await;

        let custom_nodes = self.custom_nodes_path.clone();
        let name_clone = name.to_string();
        tokio::task::spawn_blocking(move || {
            registry::toggle_plugin(&custom_nodes, &name_clone, enabled)
        })
        .await
        .map_err(|e| PluginError::IoError(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("toggle task panicked: {}", e),
        )))??;

        self.invalidate_list_cache();
        self.event_bus.emit(SystemEvent::PluginListChanged);

        Ok(())
    }

    /// 安装插件的 requirements.txt
    ///
    /// - 无 requirements.txt → Ok（视为已满足）
    /// - venv 未就绪 → VenvNotReady
    /// - pip install 失败 → RequirementsFailed（不影响插件本身可用）
    pub async fn install_requirements(&self, name: &str) -> Result<(), PluginError> {
        let venv_path = &self.venv_path;
        if venv_path.as_os_str().is_empty() || !venv_path.exists() {
            return Err(PluginError::VenvNotReady);
        }

        let plugin_path = registry::plugin_dir_path(&self.custom_nodes_path, name)
            .ok_or_else(|| PluginError::NotFound(name.to_string()))?;
        let requirements_file = plugin_path.join("requirements.txt");
        if !requirements_file.exists() {
            return Ok(()); // 无 requirements 视为已满足
        }

        let venv_python = venv_python_binary(venv_path);
        tracing::info!(name, ?requirements_file, "installing plugin requirements");

        let output = tokio::process::Command::new(&venv_python)
            .args(["-m", "pip", "install", "-r"])
            .arg(&requirements_file)
            .output()
            .await
            .map_err(|e| PluginError::RequirementsFailed {
                detail: format!("spawn failed: {}", e),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(PluginError::RequirementsFailed { detail: stderr });
        }

        Ok(())
    }

    /// 检查所有插件的远程更新
    ///
    /// 仅读检查（git fetch + 比较 commit），不修改本地。
    pub async fn check_updates(&self) -> Result<Vec<PluginUpdateInfo>, PluginError> {
        let list = self.list_plugins(false).await?;
        let mut results = Vec::with_capacity(list.plugins.len());

        for plugin in &list.plugins {
            let path = match registry::plugin_dir_path(&self.custom_nodes_path, &plugin.name) {
                Some(p) => p,
                None => continue,
            };

            let has_update = tokio::task::spawn_blocking(move || -> Result<bool, PluginError> {
                let repo = git2::Repository::open(&path)?;
                git_ops::check_remote_has_update(&repo)
            })
            .await
            .map_err(|e| PluginError::GitError(git2::Error::from_str(&format!(
                "check_updates task panicked: {}",
                e
            ))))?;

            let has_update = match has_update {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(name = %plugin.name, error = %e, "failed to check updates");
                    false
                }
            };

            results.push(PluginUpdateInfo {
                name: plugin.name.clone(),
                has_update,
                current_commit: plugin.current_commit.clone(),
                latest_commit: None, // 完整实现需读 remote ref
            });
        }

        Ok(results)
    }

    /// 获取单个插件信息
    pub async fn get_plugin_info(&self, name: &str) -> Result<PluginInfo, PluginError> {
        // 1. 缓存命中直接返回
        {
            let cache = self.list_cache.read();
            if let Some(c) = cache.as_ref() {
                if c.is_fresh() {
                    if let Some(p) = c.result.plugins.iter().find(|p| p.name == name) {
                        return Ok(p.clone());
                    }
                }
            }
        }

        // 2. 重新扫描
        let list = self.list_plugins(true).await?;
        list.plugins
            .into_iter()
            .find(|p| p.name == name)
            .ok_or_else(|| PluginError::NotFound(name.to_string()))
    }

    /// 获取插件级锁（按需创建，sync 函数）
    ///
    /// 返回 `Arc<Mutex<()>>`，调用方需 `.lock().await` 获取 guard。
    /// guard 在作用域结束时自动释放。
    fn get_plugin_lock(&self, name: &str) -> Arc<Mutex<()>> {
        self.plugin_locks
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// 失效列表缓存
    fn invalidate_list_cache(&self) {
        let mut cache = self.list_cache.write();
        *cache = None;
    }

    /// 清理 plugin_locks 中已不存在的插件条目
    ///
    /// 在 list_plugins 完成后调用，避免长期运行累积废弃条目。
    fn cleanup_plugin_locks(&self) {
        let cache = self.list_cache.read();
        if let Some(c) = cache.as_ref() {
            let active: std::collections::HashSet<String> =
                c.result.plugins.iter().map(|p| p.name.clone()).collect();
            drop(cache);
            self.plugin_locks.retain(|name, _| active.contains(name));
        }
    }
}

/// 读取 venv 的 python 二进制路径（跨平台）
///
/// - Windows: `<venv>/Scripts/python.exe`
/// - Unix: `<venv>/bin/python`
fn venv_python_binary(venv_path: &Path) -> PathBuf {
    if cfg!(windows) {
        venv_path.join("Scripts").join("python.exe")
    } else {
        venv_path.join("bin").join("python")
    }
}

/// 简单读插件描述（与 registry::read_description 同实现，独立函数避免循环依赖）
fn read_description_safe(plugin_path: &Path) -> Option<String> {
    // 优先读 pyproject.toml
    let pyproject = plugin_path.join("pyproject.toml");
    if pyproject.exists() {
        if let Ok(content) = std::fs::read_to_string(&pyproject) {
            for line in content.lines() {
                let trimmed = line.trim();
                if let Some(rest) = trimmed.strip_prefix("description") {
                    let rest = rest.trim_start();
                    if let Some(rest) = rest.strip_prefix('=') {
                        let rest = rest.trim();
                        let desc = rest.trim_matches('"').trim_matches('\'').trim();
                        if !desc.is_empty() {
                            return Some(desc.to_string());
                        }
                    }
                }
            }
        }
    }
    // fallback: __init__.py 第一个 docstring
    let init_py = plugin_path.join("__init__.py");
    if init_py.exists() {
        if let Ok(content) = std::fs::read_to_string(&init_py) {
            let triple_quote = "\"\"\"";
            if let Some(start) = content.find(triple_quote) {
                let rest = &content[start + 3..];
                if let Some(end) = rest.find(triple_quote) {
                    let desc = rest[..end].trim();
                    if !desc.is_empty() {
                        return Some(desc.to_string());
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_service(tmp: &Path) -> PluginManagerService {
        let custom_nodes = tmp.join("custom_nodes");
        std::fs::create_dir_all(&custom_nodes).unwrap();
        let venv_path = tmp.join("venv");
        std::fs::create_dir_all(&venv_path).unwrap();
        let event_bus = EventBus::new();
        PluginManagerService::new(custom_nodes, venv_path, event_bus)
    }

    fn make_local_git_repo(parent: &Path, name: &str) -> PathBuf {
        let repo_dir = parent.join(name);
        std::fs::create_dir_all(&repo_dir).unwrap();
        let repo = git2::Repository::init(&repo_dir).unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test").unwrap();
        config.set_str("user.email", "test@test.com").unwrap();
        std::fs::write(repo_dir.join("README.md"), "# test\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README.md")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();
        repo_dir
    }

    #[tokio::test]
    async fn test_list_plugins_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());
        let result = svc.list_plugins(false).await.unwrap();
        assert!(result.plugins.is_empty());
    }

    #[tokio::test]
    async fn test_list_plugins_cache_hit() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());

        // 创建一个插件目录
        std::fs::create_dir_all(tmp.path().join("custom_nodes").join("test-plugin")).unwrap();
        std::fs::write(
            tmp.path().join("custom_nodes").join("test-plugin").join("__init__.py"),
            "# test\n",
        )
        .unwrap();

        let r1 = svc.list_plugins(false).await.unwrap();
        let r2 = svc.list_plugins(false).await.unwrap();
        // 缓存命中 - fetched_at 应相同
        assert_eq!(r1.fetched_at, r2.fetched_at);
    }

    #[tokio::test]
    async fn test_list_plugins_force_refresh() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());

        std::fs::create_dir_all(tmp.path().join("custom_nodes").join("p1")).unwrap();
        std::fs::write(
            tmp.path().join("custom_nodes").join("p1").join("__init__.py"),
            "# test\n",
        )
        .unwrap();

        let _r1 = svc.list_plugins(false).await.unwrap();

        // 添加新插件
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::create_dir_all(tmp.path().join("custom_nodes").join("p2")).unwrap();
        std::fs::write(
            tmp.path().join("custom_nodes").join("p2").join("__init__.py"),
            "# test\n",
        )
        .unwrap();

        let r2 = svc.list_plugins(true).await.unwrap();
        assert_eq!(r2.plugins.len(), 2);
    }

    #[tokio::test]
    async fn test_get_plugin_info_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());
        let result = svc.get_plugin_info("nonexistent").await;
        assert!(matches!(result, Err(PluginError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_get_plugin_info_found() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());

        let plugin_dir = tmp.path().join("custom_nodes").join("found-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("__init__.py"), "# test\n").unwrap();

        let info = svc.get_plugin_info("found-plugin").await.unwrap();
        assert_eq!(info.name, "found-plugin");
        assert!(info.enabled);
    }

    #[tokio::test]
    async fn test_toggle_disable_then_enable() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());

        let plugin_dir = tmp.path().join("custom_nodes").join("toggle-test");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("__init__.py"), "# test\n").unwrap();

        svc.toggle("toggle-test", false).await.unwrap();
        assert!(tmp.path().join("custom_nodes/toggle-test.disabled").exists());

        svc.toggle("toggle-test", true).await.unwrap();
        assert!(tmp.path().join("custom_nodes/toggle-test").exists());
        assert!(!tmp.path().join("custom_nodes/toggle-test.disabled").exists());
    }

    #[tokio::test]
    async fn test_toggle_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());

        let plugin_dir = tmp.path().join("custom_nodes").join("idempotent");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("__init__.py"), "# test\n").unwrap();

        // 已启用，再启用 → 幂等
        svc.toggle("idempotent", true).await.unwrap();
        assert!(tmp.path().join("custom_nodes/idempotent").exists());

        // 禁用后再禁用 → 幂等
        svc.toggle("idempotent", false).await.unwrap();
        svc.toggle("idempotent", false).await.unwrap();
        assert!(tmp.path().join("custom_nodes/idempotent.disabled").exists());
    }

    #[tokio::test]
    async fn test_uninstall_moves_to_trash() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());

        let plugin_dir = tmp.path().join("custom_nodes").join("uninstall-me");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("__init__.py"), "# test\n").unwrap();

        let result = svc.uninstall("uninstall-me").await.unwrap();
        assert!(result.recoverable);
        assert!(!plugin_dir.exists());
        assert!(result.moved_to.exists());
    }

    #[tokio::test]
    async fn test_uninstall_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());
        let result = svc.uninstall("nonexistent").await;
        assert!(matches!(result, Err(PluginError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_install_already_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());

        // 预创建同名插件目录
        let plugin_dir = tmp.path().join("custom_nodes").join("existing-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("__init__.py"), "# test\n").unwrap();

        // 先 list 一次填充缓存
        svc.list_plugins(true).await.unwrap();

        let result = svc
            .install(
                "https://github.com/test/existing-plugin",
                |_| {},
            )
            .await;
        assert!(matches!(result, Err(PluginError::AlreadyExists(_))));
    }

    #[tokio::test]
    async fn test_install_invalid_url_protocol() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());
        let result = svc.install("file:///etc/passwd", |_| {}).await;
        assert!(matches!(result, Err(PluginError::InvalidUrl(_))));
    }

    #[tokio::test]
    async fn test_install_url_with_credentials_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());
        let result = svc
            .install("https://token@github.com/user/repo", |_| {})
            .await;
        assert!(matches!(result, Err(PluginError::InvalidUrl(_))));
    }

    #[tokio::test]
    async fn test_install_requirements_no_venv() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_nodes = tmp.path().join("custom_nodes");
        std::fs::create_dir_all(&custom_nodes).unwrap();
        let venv_path = PathBuf::new(); // 空 venv
        let svc = PluginManagerService::new(custom_nodes, venv_path, EventBus::new());

        let result = svc.install_requirements("any-plugin").await;
        assert!(matches!(result, Err(PluginError::VenvNotReady)));
    }

    #[tokio::test]
    async fn test_install_from_local_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let _svc = make_service(tmp.path());

        // 创建本地源仓库
        let src_repo = make_local_git_repo(tmp.path(), "src-repo");

        // 用 file:// 协议克隆本地仓库（仅测试，绕过 https 校验会失败）
        // 但 validate_git_url 拒绝 file://，所以这里直接调底层 clone
        // Windows 路径需转换为正斜杠并使用三斜杠 file:///<path> 格式
        let path_str = src_repo.to_string_lossy().replace('\\', "/");
        let url = format!("file:///{}", path_str);
        let target = tmp.path().join("custom_nodes").join("cloned-plugin");
        let target_for_assert = target.clone();
        let repo = tokio::task::spawn_blocking(move || {
            git_ops::clone_plugin_repo(&url, &target)
        })
        .await
        .unwrap()
        .unwrap();

        assert!(target_for_assert.exists());
        assert!(target_for_assert.join(".git").exists());
        let commit = git_ops::current_commit(&repo).unwrap();
        assert_eq!(commit.len(), 40);
    }

    #[tokio::test]
    async fn test_event_emitted_on_uninstall() {
        let tmp = tempfile::tempdir().unwrap();
        let bus = EventBus::new();
        let mut rx = bus.subscribe();

        let custom_nodes = tmp.path().join("custom_nodes");
        std::fs::create_dir_all(&custom_nodes).unwrap();
        let venv_path = tmp.path().join("venv");
        std::fs::create_dir_all(&venv_path).unwrap();

        let plugin_dir = custom_nodes.join("emit-test");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("__init__.py"), "# test\n").unwrap();

        let svc = PluginManagerService::new(custom_nodes, venv_path, bus);
        svc.uninstall("emit-test").await.unwrap();

        let event = rx.recv().await.unwrap();
        assert!(matches!(event, SystemEvent::PluginListChanged));
    }
}
