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

use crate::config::ConfigService;
use crate::event_bus::{EventBus, SystemEvent};
use crate::log_store::{LogEntry, LogLevel, LogStoreService};
use tauri::Emitter;

use self::url_util::sanitize_url_for_log;

pub mod git_ops;
pub mod models;
pub mod registry;
pub mod trash;
pub mod url_util;
pub mod venv_health;
pub mod comfyui_core;

pub use models::{
    LocalRefInfo, PluginError, PluginInfo, PluginListResult, PluginProgress, PluginUpdateInfo,
    RemoteTagInfo, SwitchResult, UninstallResult, UpdateResult,
};
pub use url_util::{derive_plugin_name, validate_git_url};

/// 列表缓存 30 秒 TTL
const LIST_CACHE_TTL: Duration = Duration::from_secs(30);

/// 解析 uv 输出 "Resolved N packages" / "Downloading N packages" 等
///
/// 匹配模式：`Resolved 14 packages`（不区分大小写，允许前后有 ANSI 颜色码）
///
/// **v3.x**：仅作 fallback，主解析走 pip 格式（见 `parse_pip_collected`）
fn parse_uv_resolved_count(line: &str) -> Option<u32> {
    let stripped = strip_ansi(line);
    let lower = stripped.to_lowercase();
    for prefix in ["resolved ", "downloading "] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let token = rest.split_whitespace().next().unwrap_or("");
            if let Ok(n) = token.parse::<u32>() {
                return Some(n);
            }
        }
    }
    None
}

/// 解析 uv 输出 "  + pkg-name==1.2.3" / "  + pkg-name"
fn parse_uv_installed_line(line: &str) -> bool {
    let stripped = strip_ansi(line);
    let trimmed = stripped.trim_start();
    if let Some(rest) = trimmed.strip_prefix('+') {
        let first = rest.split_whitespace().next().unwrap_or("");
        return !first.is_empty() && first.chars().next().map_or(false, |c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    }
    false
}

// ============ v3.x：pip 输出解析（主策略） ============
//
// **问题背景**：原先用 uv 解析，但实际走的是 `python -m pip install`，
// pip 输出格式完全不同，导致进度条始终 0%。
//
// **pip 关键行**：
// - "Installing collected packages: A, B, C, ..." → total = 包数
// - "Successfully uninstalled XXX-Y.Z.Z" → uninstalled += 1
// - "Successfully installed A B C ..." → installed = total（100%）
//
// **回退策略**：
// - uv 的 `parse_uv_*` 函数保留作为 v3.x fallback
// - 主循环优先匹配 pip 格式；不匹配时尝试 uv 格式
// - 都失败时启用时间 fallback（每 N 秒推一次）

/// 解析 pip 输出 "Installing collected packages: A, B, C"
///
/// 返回总包数（按 `,` 分割的数量）
fn parse_pip_collected(line: &str) -> Option<u32> {
    let stripped = strip_ansi(line).trim().to_string();
    // 兼容 ANSI 颜色码：去前缀 → 找 "Installing collected packages:"
    let lower = stripped.to_lowercase();
    let idx = lower.find("installing collected packages:")?;
    let after = &stripped[idx + "installing collected packages:".len()..];
    let after = after.trim();
    if after.is_empty() {
        return None;
    }
    // 按逗号分割
    let count = after.split(',').filter(|s| !s.trim().is_empty()).count();
    Some(count as u32)
}

/// 解析 pip 输出 "Successfully uninstalled XXX-X.Y.Z"
fn parse_pip_uninstalled(line: &str) -> bool {
    let stripped = strip_ansi(line).trim().to_string();
    let lower = stripped.to_lowercase();
    lower.starts_with("successfully uninstalled")
}

/// 解析 pip 输出 "Successfully installed A B C ..."
fn parse_pip_installed(line: &str) -> bool {
    let stripped = strip_ansi(line).trim().to_string();
    let lower = stripped.to_lowercase();
    lower.starts_with("successfully installed")
}

/// 去掉 ANSI 转义码（uv 的输出可能带颜色码）
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_esc = false;
    for ch in s.chars() {
        if ch == '\x1b' {
            in_esc = true;
            continue;
        }
        if in_esc {
            // ESC 序列以字母结束
            if ch.is_ascii_alphabetic() {
                in_esc = false;
            }
            continue;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    // ===== uv fallback 解析 =====
    #[test]
    fn test_parse_uv_resolved_count() {
        assert_eq!(parse_uv_resolved_count("Resolved 14 packages"), Some(14));
        assert_eq!(parse_uv_resolved_count("resolved 1 package"), Some(1));
        assert_eq!(parse_uv_resolved_count("\x1b[2mResolved\x1b[0m 14 packages"), Some(14));
        assert_eq!(parse_uv_resolved_count("Downloading 5 packages"), Some(5));
        assert_eq!(parse_uv_resolved_count("Installing 14 packages"), None);
        assert_eq!(parse_uv_resolved_count("random text"), None);
    }

    #[test]
    fn test_parse_uv_installed_line() {
        assert!(parse_uv_installed_line("  + aiohttp==3.9.5"));
        assert!(parse_uv_installed_line("  + yarl==1.9.4"));
        assert!(parse_uv_installed_line(" + numpy"));
        assert!(parse_uv_installed_line("  + aiohttp[speedups]==3.9.5"));
        assert!(!parse_uv_installed_line("  - aiohttp"));
        assert!(!parse_uv_installed_line("Installing collected packages: aiohttp"));
        assert!(!parse_uv_installed_line(""));
    }

    // ===== v3.x pip 主策略解析（来自真实日志） =====

    /// 测试 pip 关键行：来源于 `插件日志.log` 第 48 行
    /// `Installing collected packages: uv, urllib3, typing-extensions, toml, smmap, shellingham, ...`
    #[test]
    fn test_parse_pip_collected_real_log() {
        let line = "Installing collected packages: uv, urllib3, typing-extensions, toml, smmap, shellingham, safetensors, regex, pyyaml, pyjwt, pygments, pycparser, packaging, numpy, mdurl, idna, hf-xet, h11, fsspec, filelock, colorama, charset_normalizer, chardet, certifi, annotated-doc, tqdm, requests, markdown-it-py, httpcore, gitdb, click, cffi, anyio, rich, pynacl, pygit2, httpx, GitPython, cryptography, typer, huggingface-hub, tokenizers, PyGithub, transformers";
        assert_eq!(parse_pip_collected(line), Some(45));
    }

    /// 简短包列表
    #[test]
    fn test_parse_pip_collected_short() {
        assert_eq!(parse_pip_collected("Installing collected packages: aiohttp, yarl"), Some(2));
        assert_eq!(parse_pip_collected("Installing collected packages: foo"), Some(1));
    }

    /// 容错：末尾空格 / 末尾逗号
    #[test]
    fn test_parse_pip_collected_edge_cases() {
        assert_eq!(parse_pip_collected("Installing collected packages: a, b, c, "), Some(3));
        assert_eq!(parse_pip_collected("Installing collected packages: a, b, c"), Some(3));
        // 大小写
        assert_eq!(parse_pip_collected("installing collected packages: a, b"), Some(2));
        // 不匹配
        assert_eq!(parse_pip_collected("random text"), None);
        assert_eq!(parse_pip_collected("Installing collected packages:"), None);
    }

    /// 来自真实日志第 124 行
    /// `[INFO]   Successfully uninstalled charset-normalizer-2.1.1`
    #[test]
    fn test_parse_pip_uninstalled_real_log() {
        assert!(parse_pip_uninstalled("  Successfully uninstalled charset-normalizer-2.1.1"));
        assert!(parse_pip_uninstalled("Successfully uninstalled numpy-2.4.6"));
        assert!(parse_pip_uninstalled("\x1b[32mSuccessfully uninstalled xxx-1.0\x1b[0m"));
        // 否定
        assert!(!parse_pip_uninstalled("Attempting uninstall: numpy"));
        assert!(!parse_pip_uninstalled("Found existing installation: numpy 2.4.6"));
        assert!(!parse_pip_uninstalled("Uninstalling numpy-2.4.6:"));
    }

    #[test]
    fn test_parse_pip_installed() {
        assert!(parse_pip_installed("Successfully installed aiohttp yarl numpy"));
        assert!(parse_pip_installed("Successfully installed aiohttp-3.9.5 yarl-1.9.4"));
        // 否定
        assert!(!parse_pip_installed("Successfully uninstalled numpy"));
        assert!(!parse_pip_installed("Installing collected packages: a, b"));
    }

    #[test]
    fn test_strip_ansi() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(strip_ansi("plain"), "plain");
        // pip 真实输出含颜色码
        assert_eq!(
            strip_ansi("\x1b[0mInstalling collected packages: a, b\x1b[0m"),
            "Installing collected packages: a, b"
        );
    }
}

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
///
/// **路径热加载**：`custom_nodes_path` 和 `venv_path` 每次需要时从 ConfigService
/// 读取最新的 `paths.comfyui_root` / `paths.venv_path`，实现"修改 config 后无需重启立即生效"。
pub struct PluginManagerService {
    /// Config 共享引用（提供 `paths.comfyui_root` / `paths.venv_path` 热读取）
    config: Arc<ConfigService>,
    event_bus: EventBus,
    /// 插件级互斥锁（按插件名分桶）
    plugin_locks: DashMap<String, Arc<Mutex<()>>>,
    /// 列表缓存
    list_cache: RwLock<Option<ListCache>>,
    /// v3.x：LogStore 注入，用于把 install 时的 uv/pip 输出写入持久化日志
    /// 同时供 `process_logs` / `plugin_progress_log` 事件 emit。
    log_store: Option<Arc<LogStoreService>>,
    /// v3.x：Tauri AppHandle 注入，用于 emit `plugin_progress_log` 事件
    app_handle: Option<tauri::AppHandle>,
}

impl PluginManagerService {
    pub fn new(config: Arc<ConfigService>, event_bus: EventBus) -> Self {
        Self {
            config,
            event_bus,
            plugin_locks: DashMap::new(),
            list_cache: RwLock::new(Option::None),
            log_store: Option::None,
            app_handle: Option::None,
        }
    }

    /// v3.x：注入 LogStore 与 AppHandle
    ///
    /// **调用时机**：AppState 构造后启动期一次（见 `app_state.rs`）。
    /// 不在 `new` 里接受，避免循环依赖。
    pub fn with_emit(
        mut self,
        log_store: Arc<LogStoreService>,
        app_handle: tauri::AppHandle,
    ) -> Self {
        self.log_store = Some(log_store);
        self.app_handle = Some(app_handle);
        self
    }

    /// 读取当前 custom_nodes 路径（每次调用读最新 config）
    ///
    /// v3.x：优先用 `custom_nodes_path`，fallback 到 `<comfyui_root>/custom_nodes`
    /// （用户配置 custom_nodes 在 ComfyUI 外时，custom_nodes_path 会被设置）
    pub fn current_custom_nodes_path(&self) -> PathBuf {
        let cfg = self.config.get();
        cfg.paths.custom_nodes_path
            .clone()
            .unwrap_or_else(|| cfg.paths.comfyui_root.join("custom_nodes"))
    }

    /// 读取当前 venv 路径（每次调用读最新 config）
    ///
    /// v3.x：pub 出来供 commands::plugin_manager 的 venv 健康检查使用
    pub fn current_venv_path(&self) -> PathBuf {
        self.config.get().paths.venv_path.clone()
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

        // 2. spawn_blocking 扫描（路径热加载：读最新 custom_nodes_path）
        let custom_nodes = self.current_custom_nodes_path();
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

    /// 列出远程仓库的 tag 列表（不下载整个仓库，仅 ls-remote）
    ///
    /// 用于安装前让用户选择 tag 版本。
    /// 返回按名称降序排列（新版本在前）。
    pub async fn list_remote_tags(
        &self,
        url: &str,
    ) -> Result<Vec<models::RemoteTagInfo>, PluginError> {
        validate_git_url(url)?;
        let url_clone = url.to_string();
        tokio::task::spawn_blocking(move || git_ops::list_remote_tags(&url_clone))
            .await
            .map_err(|e| PluginError::CloneFailed {
                stderr: format!("list_remote_tags task panicked: {}", e),
            })?
    }

    /// 安装插件（git clone，可选 checkout 到指定 tag）
    ///
    /// 流程：
    /// 1. 校验 URL（仅 https://）
    /// 2. derive_plugin_name → 检查是否已存在
    /// 3. with_plugin_lock
    /// 4. spawn_blocking(git2 clone)
    /// 5. 如果 tag 参数存在 → checkout 到该 tag（detached HEAD）
    /// 6. 读 __init__.py / pyproject.toml 取描述
    /// 7. 检查 requirements.txt → install_requirements
    /// 8. invalidate_list_cache + emit(PluginListChanged)
    pub async fn install<F>(
        &self,
        url: &str,
        tag: Option<&str>,
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
        // 即使缓存未命中也检查文件系统（路径热加载）
        if registry::plugin_dir_path(&self.current_custom_nodes_path(), &plugin_name).is_some() {
            return Err(PluginError::AlreadyExists(plugin_name));
        }

        // 3. 持插件锁
        let lock = self.get_plugin_lock(&plugin_name);
        let _guard = lock.lock().await;

        // 4. spawn_blocking(git2 clone)（路径热加载）
        let target_dir = self.current_custom_nodes_path().join(&plugin_name);
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

        // 4.5 如果指定了 tag → checkout 到该 tag（detached HEAD）
        if let Some(tag_name) = tag {
            let target_dir_checkout = target_dir.clone();
            let tag_name_clone = tag_name.to_string();
            let checkout_result = tokio::task::spawn_blocking(move || -> Result<(), PluginError> {
                let repo = git2::Repository::open(&target_dir_checkout)?;
                git_ops::checkout_tag(&repo, &tag_name_clone)
            })
            .await
            .map_err(|e| PluginError::CloneFailed {
                stderr: format!("checkout tag task panicked: {}", e),
            })?;
            if let Err(e) = checkout_result {
                tracing::warn!(tag = tag_name, error = %e, "checkout tag failed, keeping default branch HEAD");
                progress(PluginProgress::Failed {
                    error: format!("切换到 tag {} 失败：{}（将使用默认分支）", tag_name, e),
                });
            }
        }

        // 5. 读 git 信息 + 描述（路径热加载）
        let mut info_result = tokio::task::spawn_blocking({
            let custom_nodes = self.current_custom_nodes_path();
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
                    current_ref: None,
                    backup_commit: None,
                    is_detached: false,
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
            // v3.x：发个 0% 占位，install_requirements 内部会自己 emit 进度
            progress(PluginProgress::InstallingRequirements { percent: 0 });
            match self.install_requirements(&info_result.name, false).await {
                Ok(()) => {
                    info_result.requirements_installed = true;
                    progress(PluginProgress::InstallingRequirements { percent: 100 });
                }
                Err(e) => {
                    tracing::warn!(name = %info_result.name, error = %e, "requirements install failed");
                    // 不阻塞安装成功，但通知前端依赖安装失败
                    progress(PluginProgress::Failed {
                        error: format!(
                            "插件已安装，但依赖安装失败：{}\n请稍后在插件列表中点击「装依赖」重试",
                            e
                        ),
                    });
                }
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

        let plugin_path = registry::plugin_dir_path(&self.current_custom_nodes_path(), name)
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

        let plugin_path = registry::plugin_dir_path(&self.current_custom_nodes_path(), name)
            .ok_or_else(|| PluginError::NotFound(name.to_string()))?;

        let custom_nodes = self.current_custom_nodes_path();
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

        let custom_nodes = self.current_custom_nodes_path();
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
    ///
    /// **v3.x 改造**：原实现用 `Command::output()` 一次性等子进程结束，期间
    /// 前端看不到任何输出。新实现：
    /// 1. 配 `stdout`/`stderr` piped
    /// 2. 异步读 stdout/stderr 行 → 写入 LogStore + emit `plugin_progress_log` 事件
    /// 3. 解析 uv 输出算 percent → 调 `progress` 回调推 `PluginProgress::InstallingRequirements`
    /// 4. 子进程退出后返回 ExitCode
    ///
    /// **进度解析**：
    /// - uv 输出 `Resolved N packages` → 记录 `total = N`
    /// - uv 输出 `  + pkg-name==X.Y.Z` → `installed_count += 1`
    /// - percent = `installed_count * 100 / total`（每 5% 推送一次，避免事件风暴）
    /// - 解析失败时 percent 走 fallback 模式（10/30/60/95/100）
    pub async fn install_requirements(
        &self,
        name: &str,
        force_reinstall: bool,
    ) -> Result<(), PluginError> {
        // 路径热加载：读最新 venv_path 和 custom_nodes_path
        let venv_path = self.current_venv_path();
        if venv_path.as_os_str().is_empty() || !venv_path.exists() {
            return Err(PluginError::VenvNotReady);
        }

        // v3.x 防御：装依赖前清理 site-packages 中的 `~xxx*` 损坏目录
        // **背景**：pip 在异常情况下会留下 `~xxx` 临时目录（实测：`~afetensors-0.8.0.dist-info`），
        // 后续 pip install 会 WARNING 跳过，导致 safetensors 等包安装不完整。
        // 在这里主动清理，确保本次 install 不会受历史残留影响。
        let site_packages = venv_health::site_packages_path(&venv_path);
        if site_packages.exists() {
            match venv_health::clean_broken_distributions(&site_packages) {
                Ok(removed) if !removed.is_empty() => {
                    tracing::warn!(
                        plugin = name,
                        count = removed.len(),
                        "pre-install: cleaned broken distributions from site-packages"
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "pre-install: clean_broken_distributions failed");
                }
            }
        }

        let plugin_path = registry::plugin_dir_path(&self.current_custom_nodes_path(), name)
            .ok_or_else(|| PluginError::NotFound(name.to_string()))?;
        let requirements_file = plugin_path.join("requirements.txt");
        if !requirements_file.exists() {
            return Ok(()); // 无 requirements 视为已满足
        }

        let venv_python = venv_python_binary(&venv_path);
        tracing::info!(name, ?requirements_file, "installing plugin requirements (streaming)");

        // v3.3：在 Windows 上加 CREATE_NO_WINDOW，避免弹 cmd 窗口
        // v3.x：配 stdout/stderr piped + 强制无缓冲
        // v3.x：切版本时 force_reinstall=true → 加 --force-reinstall
        let mut cmd = crate::common::process_util::new_command(&venv_python);
        cmd.args(["-u", "-m", "pip", "install", "-r"])
            .arg(&requirements_file)
            .env("PYTHONUNBUFFERED", "1")
            .env("PIP_DISABLE_PIP_VERSION_CHECK", "1")
            .env("PIP_NO_COLOR", "1")
            .env("PIP_PROGRESS_BAR", "off")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if force_reinstall {
            cmd.arg("--force-reinstall");
        }

        let mut child = cmd.spawn().map_err(|e| PluginError::RequirementsFailed {
            detail: format!("spawn failed: {}", e),
        })?;

        let stdout = child.stdout.take().ok_or_else(|| PluginError::RequirementsFailed {
            detail: "failed to capture stdout".to_string(),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| PluginError::RequirementsFailed {
            detail: "failed to capture stderr".to_string(),
        })?;

        // 流式读 stdout，解析 + 推日志 + 推 percent
        let log_store = self.log_store.clone();
        let app_handle = self.app_handle.clone();
        let plugin_name = name.to_string();
        let stdout_task = tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            let start_time = std::time::Instant::now();
            // v3.x 状态机
            let mut total: u32 = 0;          // 解析到的总包数（pip collected 或 uv resolved）
            let mut done_count: u32 = 0;     // 已完成的包数（pip uninstalled + installed）
            let mut last_pushed_percent: i32 = -1;
            // v3.x 模式标识
            let mut mode: &str = "unknown";  // "pip" | "uv" | "unknown"

            while let Ok(Some(line)) = lines.next_line().await {
                // ====== v3.x 进度解析（双策略） ======

                // 1) pip 格式（主策略）
                if let Some(n) = parse_pip_collected(&line) {
                    total = n;
                    mode = "pip";
                    tracing::debug!(plugin = %plugin_name, total, mode, "pip collected");
                }
                if parse_pip_uninstalled(&line) {
                    done_count += 1;
                    if mode == "unknown" {
                        mode = "pip";
                    }
                }
                if parse_pip_installed(&line) {
                    // pip install 完成：直接推 100%
                    done_count = total.max(done_count);
                    mode = "pip";
                }

                // 2) uv 格式（fallback）
                if total == 0 {
                    if let Some(n) = parse_uv_resolved_count(&line) {
                        total = n;
                        mode = "uv";
                        tracing::debug!(plugin = %plugin_name, total, mode, "uv resolved");
                    }
                }
                if parse_uv_installed_line(&line) {
                    done_count += 1;
                    if mode == "unknown" {
                        mode = "uv";
                    }
                }

                // 3) 算 percent
                let mut pct: i32 = if total > 0 {
                    (done_count * 100 / total) as i32
                } else {
                    // 解析失败 → 时间 fallback：30s→30%, 60s→60%, 90s→90%
                    let elapsed = start_time.elapsed().as_secs();
                    if elapsed < 30 {
                        0
                    } else if elapsed < 60 {
                        30
                    } else if elapsed < 90 {
                        60
                    } else {
                        90
                    }
                };

                // 钳位 [0, 100]
                if pct < 0 {
                    pct = 0;
                } else if pct > 100 {
                    pct = 100;
                }

                // 4) 推事件（节流：每 2% 推一次，或 100%）
                if pct >= last_pushed_percent as i32 + 2 || pct == 100 {
                    last_pushed_percent = pct;
                    if let Some(app) = &app_handle {
                        let _ = app.emit(
                            "plugin_progress",
                            &PluginProgress::RequirementsPercent {
                                plugin: plugin_name.clone(),
                                percent: pct as u32,
                            },
                        );
                    }
                }

                // 5) 写 LogStore + emit 实时日志
                let level = if line.to_lowercase().contains("error") {
                    LogLevel::Error
                } else if line.to_lowercase().contains("warning") {
                    LogLevel::Warn
                } else {
                    LogLevel::Info
                };
                if let Some(ls) = &log_store {
                    let entry = LogEntry {
                        timestamp: chrono::Utc::now(),
                        level,
                        source: format!("plugin:{}", plugin_name),
                        message: line.clone(),
                    };
                    // 异步写（不阻塞 stream 读取）
                    let ls_clone = ls.clone();
                    let entry_clone = entry.clone();
                    tokio::spawn(async move {
                        let _ = ls_clone.logs().append(entry_clone).await;
                    });
                }
                // emit 给前端（plugin_progress_log）
                if let Some(app) = &app_handle {
                    let _ = app.emit(
                        "plugin_progress_log",
                        &serde_json::json!({
                            "plugin": plugin_name,
                            "level": level.as_str(),
                            "message": line,
                        }),
                    );
                }
            }
        });

        // 流式读 stderr（uv 的 warning 走这里）
        let log_store_err = self.log_store.clone();
        let app_handle_err = self.app_handle.clone();
        let plugin_name_err = name.to_string();
        let stderr_task = tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(ls) = &log_store_err {
                    let entry = LogEntry {
                        timestamp: chrono::Utc::now(),
                        level: LogLevel::Warn,
                        source: format!("plugin:{}", plugin_name_err),
                        message: line.clone(),
                    };
                    let ls_clone = ls.clone();
                    let entry_clone = entry.clone();
                    tokio::spawn(async move {
                        let _ = ls_clone.logs().append(entry_clone).await;
                    });
                }
                if let Some(app) = &app_handle_err {
                    let _ = app.emit(
                        "plugin_progress_log",
                        &serde_json::json!({
                            "plugin": plugin_name_err,
                            "level": "warn",
                            "message": line,
                        }),
                    );
                }
            }
        });

        // 等子进程退出
        let status = child.wait().await.map_err(|e| PluginError::RequirementsFailed {
            detail: format!("wait failed: {}", e),
        })?;

        // 等 stdout/stderr 读完（避免漏行）
        let _ = stdout_task.await;
        let _ = stderr_task.await;

        if !status.success() {
            return Err(PluginError::RequirementsFailed {
                detail: format!("pip exited with code {:?}", status.code()),
            });
        }

        // 推 100% 事件（兜底，解析失败时也保证最终状态正确）
        if let Some(app) = &self.app_handle {
            let _ = app.emit(
                "plugin_progress",
                &PluginProgress::InstallingRequirements { percent: 100 },
            );
        }

        // v3.x 验证：pip 退出 0 不代表包装完整
        // 背景：pip 跳过 `~xxx*` 损坏包时**不会报错**，但 safetensors 0.8.0 等
        // 就会缺失 torch.py / numpy.py 等子模块。这里跑一个 import 链验证：
        // 1. 只验证**不依赖**该 plugin 的核心 import（safetensors.torch / folder_paths 等）
        // 2. 失败时**只警告不报错**（避免阻断正常 install 流程）
        // 3. 详细失败信息写到 LogStore + emit 一个 `venv_import_warning` 事件给前端
        let venv_python = venv_python_binary(&venv_path);
        if venv_python.exists() {
            let venv_python_for_check = venv_python.clone();
            let import_results =
                venv_health::verify_critical_imports(&venv_python_for_check).await;
            let failed: Vec<_> = import_results.iter().filter(|r| !r.ok).collect();
            if !failed.is_empty() {
                let summary = failed
                    .iter()
                    .map(|r| {
                        format!(
                            "{}: {}",
                            r.module,
                            r.error.as_deref().unwrap_or("(no detail)")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                tracing::warn!(
                    plugin = name,
                    failed_count = failed.len(),
                    total = import_results.len(),
                    "post-install: critical imports failed, venv may be broken"
                );
                // 写 LogStore
                if let Some(ls) = &self.log_store {
                    let entry = LogEntry {
                        timestamp: chrono::Utc::now(),
                        level: LogLevel::Warn,
                        source: format!("plugin:{}", name),
                        message: format!("venv 健康检查失败（pip 退出 0 但关键 import 不可用）: {}", summary),
                    };
                    let ls_clone = ls.clone();
                    tokio::spawn(async move {
                        let _ = ls_clone.logs().append(entry).await;
                    });
                }
                // emit 给前端
                if let Some(app) = &self.app_handle {
                    let _ = app.emit(
                        "venv_import_warning",
                        &serde_json::json!({
                            "plugin": name,
                            "failed_modules": failed.iter().map(|r| r.module.clone()).collect::<Vec<_>>(),
                            "summary": summary,
                        }),
                    );
                }
            }
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
            let path = match registry::plugin_dir_path(&self.current_custom_nodes_path(), &plugin.name) {
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

    // ============ v3.x：版本切换相关方法 ============

    /// 列出指定插件的可用 ref（本地 tag + branch）
    ///
    /// **流程**：
    /// 1. 加 plugin_lock（防并发）
    /// 2. open repo
    /// 3. fetch_all_tags（拉远程 tag，~1-5s）
    /// 4. list_local_refs → 返回排序后的 ref 列表
    pub async fn list_available_versions(
        &self,
        name: &str,
    ) -> Result<Vec<LocalRefInfo>, PluginError> {
        let lock = self.get_plugin_lock(name);
        let _guard = lock.lock().await;

        let plugin_path =
            registry::plugin_dir_path(&self.current_custom_nodes_path(), name)
                .ok_or_else(|| PluginError::NotFound(name.to_string()))?;

        let refs = tokio::task::spawn_blocking(move || -> Result<Vec<LocalRefInfo>, PluginError> {
            let repo = git2::Repository::open(&plugin_path)?;
            // 先 fetch 拉最新 tag
            git_ops::fetch_all_tags(&repo)?;
            git_ops::list_local_refs(&repo)
        })
        .await
        .map_err(|e| PluginError::GitError(git2::Error::from_str(&format!(
            "list_available_versions task panicked: {}",
            e
        ))))??;

        Ok(refs)
    }

    /// 切换插件到指定 ref（tag / branch / commit hash）
    ///
    /// **完整流程**：
    /// 1. 加 plugin_lock
    /// 2. emit `Switching{0}` + fetch_all_tags
    /// 3. emit `Switching{50}` + checkout_ref（带 backup_commit）
    /// 4. emit `Switching{100}`
    /// 5. 装依赖（force_reinstall=true）— 失败不回滚（让用户决定）
    /// 6. 失效缓存 + emit PluginListChanged
    /// 7. 返回 SwitchResult（含 previous_commit 供回滚按钮使用）
    ///
    /// **返回**：`SwitchResult { plugin, previous_commit, need_restart }`
    pub async fn switch_version(
        &self,
        name: &str,
        target_ref: &str,
    ) -> Result<SwitchResult, PluginError> {
        let lock = self.get_plugin_lock(name);
        let _guard = lock.lock().await;

        // 1. 推送 Switching{0}
        self.emit_progress(PluginProgress::Switching { percent: 0 });

        // 2. 解析插件路径
        let plugin_path =
            registry::plugin_dir_path(&self.current_custom_nodes_path(), name)
                .ok_or_else(|| PluginError::NotFound(name.to_string()))?;
        let target = target_ref.to_string();
        let plugin_path_clone = plugin_path.clone();
        let app = self.app_handle.clone();

        // 3. fetch + checkout
        let (previous_commit, new_commit) = tokio::task::spawn_blocking(
            move || -> Result<(String, String), PluginError> {
                let repo = git2::Repository::open(&plugin_path_clone)?;
                // emit Switching{30}
                if let Some(app) = &app {
                    let _ = app.emit(
                        "plugin_progress",
                        &PluginProgress::Switching { percent: 30 },
                    );
                }
                // 拉最新 tag
                git_ops::fetch_all_tags(&repo)?;
                if let Some(app) = &app {
                    let _ = app.emit(
                        "plugin_progress",
                        &PluginProgress::Switching { percent: 60 },
                    );
                }
                // checkout（带 backup 记录）
                let (prev, new) = git_ops::checkout_ref(&repo, &target)?;
                Ok((prev, new))
            },
        )
        .await
        .map_err(|e| PluginError::GitError(git2::Error::from_str(&format!(
            "switch_version task panicked: {}",
            e
        ))))??;

        // 4. emit Switching{100}
        self.emit_progress(PluginProgress::Switching { percent: 100 });

        tracing::info!(
            name,
            target = %target_ref,
            previous = %previous_commit,
            new = %new_commit,
            "plugin version switched"
        );

        // 5. 装依赖（force_reinstall=true）— 失败不回滚
        let has_requirements = plugin_path.join("requirements.txt").exists();
        if has_requirements {
            self.emit_progress(PluginProgress::InstallingRequirements { percent: 0 });
            match self.install_requirements(name, true).await {
                Ok(()) => {
                    self.emit_progress(PluginProgress::InstallingRequirements {
                        percent: 100,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        name,
                        error = %e,
                        "switched but requirements install failed (user can retry)"
                    );
                    self.emit_progress(PluginProgress::Failed {
                        error: format!(
                            "版本已切换到 {}，但依赖装失败：{}\n请稍后点「装依赖」重试",
                            target_ref, e
                        ),
                    });
                    // 不 return，让用户至少看到切换后的代码
                }
            }
        }

        // 5.5. 持久化 backup_commit（用于 rollback）—— 写到 `<plugin>/.launcher_backup_commit`
        if let Err(e) = write_backup_commit(&plugin_path, &previous_commit) {
            tracing::warn!(name, error = %e, "failed to persist backup_commit");
        }

        // 6. 失效缓存 + emit
        self.invalidate_list_cache();
        self.event_bus.emit(SystemEvent::PluginListChanged);

        // 7. 构造 SwitchResult
        let plugin = self.get_plugin_info(name).await?;
        let need_restart = self.is_comfyui_running().await;

        // emit Done
        self.emit_progress(PluginProgress::Done);

        Ok(SwitchResult {
            plugin,
            previous_commit,
            need_restart,
        })
    }

    /// 回滚到上次切版本前的 commit
    ///
    /// 流程：restore_commit + 装依赖（force_reinstall）
    pub async fn rollback_version(&self, name: &str) -> Result<PluginInfo, PluginError> {
        // 1. 读 backup_commit（持久化在 `<plugin>/.launcher_backup_commit`）
        let plugin_path =
            registry::plugin_dir_path(&self.current_custom_nodes_path(), name)
                .ok_or_else(|| PluginError::NotFound(name.to_string()))?;
        let backup = read_backup_commit(&plugin_path).ok_or_else(|| {
            PluginError::GitError(git2::Error::from_str(
                "no backup commit recorded, cannot rollback",
            ))
        })?;

        let lock = self.get_plugin_lock(name);
        let _guard = lock.lock().await;
        let backup_clone = backup.clone();
        let plugin_path_clone = plugin_path.clone();
        let app = self.app_handle.clone();

        // 2. restore_commit
        tokio::task::spawn_blocking(move || -> Result<(), PluginError> {
            let repo = git2::Repository::open(&plugin_path_clone)?;
            if let Some(app) = &app {
                let _ = app.emit(
                    "plugin_progress",
                    &PluginProgress::Switching { percent: 30 },
                );
            }
            git_ops::restore_commit(&repo, &backup_clone)?;
            Ok(())
        })
        .await
        .map_err(|e| PluginError::GitError(git2::Error::from_str(&format!(
            "rollback task panicked: {}",
            e
        ))))??;

        // 3. 装依赖
        if plugin_path.join("requirements.txt").exists() {
            let _ = self.install_requirements(name, true).await;
        }

        // 4. 失效 + emit
        self.invalidate_list_cache();
        self.event_bus.emit(SystemEvent::PluginListChanged);
        self.emit_progress(PluginProgress::Done);

        self.get_plugin_info(name).await
    }

    /// 推送 PluginProgress 事件（内部 helper）
    fn emit_progress(&self, p: PluginProgress) {
        if let Some(app) = &self.app_handle {
            let _ = app.emit("plugin_progress", &p);
        }
    }

    /// 读取 ComfyUI 核心目录（每次从 config 读最新）
    fn current_comfyui_root(&self) -> PathBuf {
        self.config.get().paths.comfyui_root.clone()
    }

    /// v3.x：检查 ComfyUI 核心依赖状态
    pub fn check_comfyui_requirements(
        &self,
        force_reinstall: bool,
    ) -> comfyui_core::ComfyUICoreRequirementsStatus {
        comfyui_core::check_comfyui_requirements(
            &self.current_comfyui_root(),
            &self.current_custom_nodes_path(),
            force_reinstall,
        )
    }

    /// v3.x：启动 ComfyUI 前的完整检查
    pub fn launch_pre_check(&self, force_reinstall: bool) -> comfyui_core::PreLaunchCheck {
        let custom_nodes = self.current_custom_nodes_path();
        comfyui_core::pre_launch_check(
            &self.current_comfyui_root(),
            &custom_nodes,
            force_reinstall,
            |cn_path| {
                // 复用 scan_plugins + requirements_installed 字段
                let plugins = match registry::scan_plugins(cn_path) {
                    Ok(p) => p,
                    Err(_) => return Vec::new(),
                };
                plugins
                    .into_iter()
                    .filter(|p| p.enabled && !p.requirements_installed)
                    .map(|p| comfyui_core::PluginInstallNeeded {
                        name: p.name,
                        path: cn_path.join(&p.dir_name),
                        commit: Some(p.current_commit),
                        current_ref: p.current_ref,
                    })
                    .collect()
            },
        )
    }

    /// v3.x：装 ComfyUI 核心依赖（带进度 + 日志 + 状态文件写入）
    ///
    /// **复用 plugin_install_requirements 的核心机制**：
    /// - emit PluginProgress::InstallingRequirements 事件
    /// - emit `plugin_progress_log` 事件（通过 self.app_handle）
    /// - 装完写 hash 状态文件
    pub async fn install_comfyui_requirements(
        &self,
        force_reinstall: bool,
    ) -> Result<String, PluginError> {
        let comfyui_root = self.current_comfyui_root();
        let custom_nodes = self.current_custom_nodes_path();
        let venv = self.current_venv_path();
        let requirements_path = comfyui_root.join("requirements.txt");

        if !requirements_path.exists() {
            return Err(PluginError::GitError(git2::Error::from_str(
                "ComfyUI/requirements.txt not found",
            )));
        }

        // emit InstallingRequirements 事件（让前端打开进度面板）
        self.emit_progress(PluginProgress::InstallingRequirements {
            percent: 0,
        });

        // 回调：emit 实时日志（前端从 plugin_progress_log 事件订阅）
        //
        // **设计**：
        // - LogStore 是给"启动 ComfyUI 后台运行"用的持久化（写到 SQLite）
        // - 装核心依赖是同步的（launcher 等待 pip install 完成），不写 LogStore 也 OK
        // - 前端进度面板已经从 plugin_progress_log 实时显示，足够用户看
        let app = self.app_handle.clone();
        let on_log = move |line: &str| {
            if let Some(handle) = &app {
                let _ = handle.emit(
                    "plugin_progress_log",
                    serde_json::json!({
                        "plugin": comfyui_core::COMFYUI_CORE_PLUGIN_KEY,
                        "line": line,
                    }),
                );
            }
        };

        // 调用 comfyui_core::install_comfyui_requirements
        let result = comfyui_core::install_comfyui_requirements(
            &comfyui_root,
            &custom_nodes,
            &venv,
            force_reinstall,
            on_log,
        )
        .await;

        match &result {
            Ok(hash) => {
                self.emit_progress(PluginProgress::InstallingRequirements {
                    percent: 100,
                });
                tracing::info!(
                    hash = %hash,
                    "ComfyUI 核心依赖装成功"
                );
            }
            Err(e) => {
                self.emit_progress(PluginProgress::Failed {
                    error: format!("ComfyUI 核心依赖装失败: {}", e),
                });
                tracing::error!(error = %e, "ComfyUI 核心依赖装失败");
                return Err(PluginError::GitError(git2::Error::from_str(&e.to_string())));
            }
        }

        // 装完后再 emit Done（让前端关闭进度面板）
        self.emit_progress(PluginProgress::Done);
        Ok(result.unwrap())
    }

    /// 检查 ComfyUI 是否在跑（用于 SwitchResult.need_restart）
    ///
    /// **简化实现**：探测默认端口 8188（ComfyUI 默认）。
    /// 失败时返回 false（不阻塞切换主流程）。
    async fn is_comfyui_running(&self) -> bool {
        let addr = "127.0.0.1:8188".to_string();
        let result = tokio::task::spawn_blocking(move || {
            use std::net::ToSocketAddrs;
            use std::time::Duration;
            let socket = match addr.to_socket_addrs() {
                Ok(mut iter) => match iter.next() {
                    Some(s) => s,
                    None => return Ok(false),
                },
                Err(_) => return Ok(false),
            };
            // 200ms 超时
            std::net::TcpStream::connect_timeout(&socket, Duration::from_millis(200))
                .map(|_| true)
                .map_err(|e| e)
        })
        .await;
        matches!(result, Ok(Ok(true)))
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

/// backup commit 持久化文件名（写在 `<plugin>/.launcher_backup_commit`）
const BACKUP_COMMIT_FILE: &str = ".launcher_backup_commit";

/// 读 backup_commit（从 `<plugin>/.launcher_backup_commit`）
///
/// 文件不存在或内容不是 40 位 hex 字符串时返回 None。
fn read_backup_commit(plugin_path: &Path) -> Option<String> {
    let file = plugin_path.join(BACKUP_COMMIT_FILE);
    if !file.exists() {
        return None;
    }
    std::fs::read_to_string(&file)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()))
}

/// 写 backup_commit
fn write_backup_commit(plugin_path: &Path, commit: &str) -> Result<(), std::io::Error> {
    let file = plugin_path.join(BACKUP_COMMIT_FILE);
    std::fs::write(&file, commit)
}

/// 清理 backup_commit 文件（rollback 后调用）
fn clear_backup_commit(plugin_path: &Path) -> Result<(), std::io::Error> {
    let file = plugin_path.join(BACKUP_COMMIT_FILE);
    if file.exists() {
        std::fs::remove_file(&file)?;
    }
    Ok(())
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
    use crate::config::ConfigService;

    /// 构造测试用 PluginManagerService（路径热加载版）
    ///
    /// 与 `new(config, event_bus)` 一致，但额外用 `config.update()` 把
    /// `comfyui_root` / `venv_path` 指向临时目录。
    async fn make_service(tmp: &Path) -> PluginManagerService {
        let custom_nodes = tmp.join("custom_nodes");
        std::fs::create_dir_all(&custom_nodes).unwrap();
        let venv_path = tmp.join("venv");
        std::fs::create_dir_all(&venv_path).unwrap();
        let event_bus = EventBus::new();
        let config = std::sync::Arc::new(ConfigService::new_for_test(event_bus.clone()));
        config
            .update(|cfg| {
                cfg.paths.comfyui_root = tmp.to_path_buf();
                cfg.paths.venv_path = venv_path;
                Ok(())
            })
            .await
            .expect("set paths");
        PluginManagerService::new(config, event_bus)
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
        let svc = make_service(tmp.path()).await;
        let result = svc.list_plugins(false).await.unwrap();
        assert!(result.plugins.is_empty());
    }

    #[tokio::test]
    async fn test_list_plugins_cache_hit() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path()).await;

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
        let svc = make_service(tmp.path()).await;

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
        let svc = make_service(tmp.path()).await;
        let result = svc.get_plugin_info("nonexistent").await;
        assert!(matches!(result, Err(PluginError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_get_plugin_info_found() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path()).await;

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
        let svc = make_service(tmp.path()).await;

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
        let svc = make_service(tmp.path()).await;

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
        let svc = make_service(tmp.path()).await;

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
        let svc = make_service(tmp.path()).await;
        let result = svc.uninstall("nonexistent").await;
        assert!(matches!(result, Err(PluginError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_install_already_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path()).await;

        // 预创建同名插件目录
        let plugin_dir = tmp.path().join("custom_nodes").join("existing-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("__init__.py"), "# test\n").unwrap();

        // 先 list 一次填充缓存
        svc.list_plugins(true).await.unwrap();

        let result = svc
            .install(
                "https://github.com/test/existing-plugin",
                None,
                |_| {},
            )
            .await;
        assert!(matches!(result, Err(PluginError::AlreadyExists(_))));
    }

    #[tokio::test]
    async fn test_install_invalid_url_protocol() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path()).await;
        let result = svc.install("file:///etc/passwd", None, |_| {}).await;
        assert!(matches!(result, Err(PluginError::InvalidUrl(_))));
    }

    #[tokio::test]
    async fn test_install_url_with_credentials_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path()).await;
        let result = svc
            .install("https://token@github.com/user/repo", None, |_| {})
            .await;
        assert!(matches!(result, Err(PluginError::InvalidUrl(_))));
    }

    #[tokio::test]
    async fn test_install_requirements_no_venv() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_nodes = tmp.path().join("custom_nodes");
        std::fs::create_dir_all(&custom_nodes).unwrap();
        let venv_path = PathBuf::new(); // 空 venv
        // 路径热加载版 fixture（config.comfyui_root 指向 tmp）
        let event_bus = EventBus::new();
        let config = std::sync::Arc::new(
            crate::config::ConfigService::new_for_test(event_bus.clone()),
        );
        config
            .update(|cfg| {
                cfg.paths.comfyui_root = tmp.path().to_path_buf();
                cfg.paths.venv_path = venv_path;
                Ok(())
            })
            .await
            .expect("set paths");
        let svc = PluginManagerService::new(config, event_bus);

        let result = svc.install_requirements("any-plugin", false).await;
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

        // 路径热加载版 fixture
        let config = std::sync::Arc::new(crate::config::ConfigService::new_for_test(bus.clone()));
        config
            .update(|cfg| {
                cfg.paths.comfyui_root = tmp.path().to_path_buf();
                cfg.paths.venv_path = venv_path;
                Ok(())
            })
            .await
            .expect("set paths");
        let svc = PluginManagerService::new(config, bus);
        svc.uninstall("emit-test").await.unwrap();

        let event = rx.recv().await.unwrap();
        assert!(matches!(event, SystemEvent::PluginListChanged));
    }
}
