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
use crate::python_env::torch_variant::TorchVariant;
use crate::system::gpu_cache::get_or_detect;
use crate::system::recommend::recommend_torch_variant_with_gpus;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use super::{
    migrations, Config, CudaVersion, SharedConfig, CURRENT_SCHEMA_VERSION,
    ConfigForToml,
};

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
    /// **v3.x 绿色版行为变更**：
    /// - 文件不存在时**不**自动创建 config.toml（避免"看上去配好了"的污染）
    /// - 内存中用默认 Config（paths 由 `apply_default_paths` 从 `launcher-portable.dat` 解析注入）
    /// - 第一次 `update()` 才会真正写盘
    /// - 加载现有文件时，**重新注入** paths（忽略文件里的旧值，保证跨目录分发后路径自动适配）
    ///
    /// TOML 解析失败时备份原文件 + 内存用默认配置
    pub async fn load(path: PathBuf, event_bus: EventBus) -> Result<Self, ConfigError> {
        // 确保父目录存在（v3.x：保留，让 SQLite 之类能找到 data/）
        if let Some(parent) = path.parent() {
            paths::ensure_dir(parent).await.map_err(io_err)?;
        }

        let config = if path.exists() {
            let raw_content = tokio::fs::read_to_string(&path).await.map_err(io_err)?;
            // **v3.4.1**：旧 preview_method 值（"latent" / "latent-upscale" / "autoencoder"）
            // 在新枚举中无法反序列化，会导致整个 Config 解析失败、用户配置被重置。
            // 这里先做 TOML 文本层级的预处理，把旧值替换为新值，再走正常解析流程。
            let (content, migrated_preview) = preprocess_legacy_toml(&raw_content);
            if let Some((from, to)) = migrated_preview {
                tracing::warn!(
                    from = %from,
                    to = %to,
                    "v3.4.1: 旧版 preview_method 自动迁移到 ComfyUI 实际支持的值"
                );
            }
            // 发生了迁移 → 立即把预处理后的文本写回磁盘（避免下次重复处理）
            if content != raw_content {
                save_raw_to_disk(&path, &content).await?;
            }
            match toml::from_str::<Config>(&content) {
                Ok(mut cfg) => {
                    // 自动迁移
                    if cfg.schema_version < CURRENT_SCHEMA_VERSION {
                        let from = cfg.schema_version;
                        migrations::migrate(&mut cfg, from, CURRENT_SCHEMA_VERSION)?;
                        // 迁移后立即保存
                        save_to_disk(&path, &cfg).await?;
                    }
                    // **v1.8 / F36 自动迁移**：检测 venv 路径是否在 src-tauri/ 下
                    // 若是，自动改到 app_data_dir/data/venv，并把旧 venv 复制过去
                    // 旧 venv 在 src-tauri/ 下会被 Tauri dev 监视触发 rebuild，
                    // 复制到 app_data_dir 下彻底解决问题
                    if let Err(reason) = validate_venv_path_not_under_src_tauri(&cfg.paths.venv_path) {
                        tracing::warn!(
                            old_venv = %cfg.paths.venv_path.display(),
                            reason = %reason,
                            "F36: venv 在 src-tauri/ 下，自动迁移到 app_data_dir"
                        );
                        let old_venv = cfg.paths.venv_path.clone();
                        let new_venv = paths::app_data_dir().join("data").join("venv");
                        // 若旧 venv 存在且新 venv 不存在 → 移动过去（保留用户的依赖）
                        if old_venv.exists() && !new_venv.exists() {
                            if let Err(e) = migrate_venv_dir(&old_venv, &new_venv).await {
                                tracing::error!(error = %e, "F36: 迁移 venv 失败，将创建新 venv");
                            } else {
                                tracing::info!(
                                    from = %old_venv.display(),
                                    to = %new_venv.display(),
                                    "F36: venv 迁移成功"
                                );
                            }
                        }
                        cfg.paths.venv_path = new_venv;
                        save_to_disk(&path, &cfg).await?;
                    }
                    // **v3.x 绿色版**：重新注入 paths 字段
                    //
                    // 原因：用户可能从 A 目录压缩整个环境分发到 B 目录解压启动。
                    // config.toml 里旧的 paths（来自 A 目录）已失效，必须用 B 目录重新解析。
                    // 配合 models.rs 的 `skip_serializing`，写盘时 paths 永远不会被持久化。
                    apply_default_paths(&mut cfg);
                    cfg
                }
                Err(e) => {
                    // 解析失败：备份 + 内存用默认（**v3.x：不写新文件**）
                    let backup = path.with_extension(format!("toml.corrupt-{}", chrono::Utc::now().timestamp()));
                    tracing::warn!(error = %e, ?backup, "config parse failed, backing up");
                    let _ = tokio::fs::rename(&path, &backup).await;
                    build_default_config()
                }
            }
        } else {
            // **v3.x 绿色版**：config.toml 不存在时**不**创建
            //
            // 理由：解压到新目录后，BoundLaunch 首次启动不应该"看上去配好了"。
            // 让用户认为这是全新环境，所有配置由 launcher-portable.dat 决定。
            // 等用户**首次 update** 时才真正写盘（此时用户已经显式改过配置）。
            // apply_default_paths 已经在 build_default_config 里调过了
            //
            // **v3.x 智能推荐（Phase 1）**：首次启动用智能推荐结果覆盖默认 cuda_version
            // 探测 GPU → 调 recommend_torch_variant_with_gpus → 用推荐结果
            // 不写 config.toml（用户首次 update 时才写）
            let mut cfg = build_default_config();
            let gpus = get_or_detect().await;
            let recommended = recommend_torch_variant_with_gpus(&gpus);
            apply_recommended_torch(&mut cfg, &recommended);
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
///
/// **v3.x 关键修复**：用 `ConfigForToml` 视图结构序列化，**根本不带 paths 段**。
///
/// 之前用 `toml::to_string_pretty(config)` 直接序列化 Config，但 PathsConfig 字段的
/// `#[serde(skip_serializing)]` 是字段级标记，对所有序列化器都生效（包括 serde_json），
/// 导致前端 invoke 也拿不到这些字段，路由守卫会误判。
///
/// 修复：移除字段级 skip_serializing，改用视图结构 `ConfigForToml`：
/// - 视图结构在 TOML 路径上**根本不含** paths 字段
/// - invoke 走完整 Config 序列化（带 paths）→ 前端能正常拿
/// - 两个序列化器完全解耦，不再互相影响
async fn save_to_disk(path: &Path, config: &Config) -> Result<(), ConfigError> {
    let toml_view: ConfigForToml = config.into();
    let content = toml::to_string_pretty(&toml_view)
        .map_err(|e| ConfigError::SerializeError(e.to_string()))?;
    super::atomic_write::atomic_write(path, &content)
        .await
        .map_err(|e| ConfigError::IoError(e.to_string()))
}

/// **v1.8 / F36**：把 venv 目录从 src-tauri/ 下迁移到 app_data_dir
///
/// 行为：使用 tokio::fs::rename（如果同盘）或递归复制（跨盘）
async fn migrate_venv_dir(from: &std::path::Path, to: &std::path::Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        paths::ensure_dir(parent)
            .await
            .map_err(|e| format!("创建父目录失败: {}", e))?;
    }
    // 先尝试 rename（同盘极快）
    match tokio::fs::rename(from, to).await {
        Ok(()) => return Ok(()),
        Err(_) => {
            // 跨盘 → 递归复制
            copy_dir_recursive(from, to).await?;
            // 复制成功后删旧目录
            let _ = tokio::fs::remove_dir_all(from).await;
        }
    }
    Ok(())
}

async fn copy_dir_recursive(from: &std::path::Path, to: &std::path::Path) -> Result<(), String> {
    tokio::fs::create_dir_all(to)
        .await
        .map_err(|e| format!("创建 {} 失败: {}", to.display(), e))?;
    let mut entries = tokio::fs::read_dir(from)
        .await
        .map_err(|e| format!("读取 {} 失败: {}", from.display(), e))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| e.to_string())?
    {
        let ft = entry.file_type().await.map_err(|e| e.to_string())?;
        let dest = to.join(entry.file_name());
        if ft.is_dir() {
            Box::pin(copy_dir_recursive(&entry.path(), &dest)).await?;
        } else if ft.is_symlink() {
            // 软链接：复制链接本身
            let target = tokio::fs::read_link(&entry.path())
                .await
                .map_err(|e| e.to_string())?;
            #[cfg(unix)]
            tokio::fs::symlink(&target, &dest)
                .await
                .map_err(|e| e.to_string())?;
            #[cfg(windows)]
            {
                if target.is_dir() {
                    tokio::fs::symlink_dir(&target, &dest)
                        .await
                        .map_err(|e| e.to_string())?;
                } else {
                    tokio::fs::symlink_file(&target, &dest)
                        .await
                        .map_err(|e| e.to_string())?;
                }
            }
        } else {
            tokio::fs::copy(&entry.path(), &dest)
                .await
                .map_err(|e| format!("复制 {} 失败: {}", entry.path().display(), e))?;
        }
    }
    Ok(())
}

/// 构造默认 Config，并将空路径字段填充为 launcher 工作目录
///
/// 与 `Config::default()` 区别：
/// - `Config::default()` 是无内存访问的纯默认值（空 PathBuf）
/// - `build_default_config()` 额外调用 `paths::launcher_working_dir()` 填充 comfyui_root，
///   并把 venv_path 设置为 `${comfyui_root}/venv`
fn build_default_config() -> Config {
    let mut cfg = Config::default();
    apply_default_paths(&mut cfg);
    cfg
}

/// 将空路径字段填充为 launcher 工作目录
///
/// 规则（v1.8 / F38 Portable 模式重写）：
/// - `comfyui_root` 为空 → 设置为 `<portable_base_dir>/ComfyUI`（dev 时是项目根，prod 时是 exe 旁）
/// - `venv_path` 为空 → 设置为 `<app_data_dir>/venv`（v3.1 / F26 决策 1：venv 独立于 ComfyUI 仓库）
/// - `models_path` 不在此处设置（保持 `None`，由用户在设置页显式配置）
///
/// **v3.x 增强**：启动时调 `env_paths::resolve()`：
/// - 如果 `launcher-portable.dat` 存在（绿色版模式）→ 用它定义的路径
/// - 否则（传统安装模式）→ 走原有的 v1.8 逻辑
///
/// 已配置的路径不会被覆盖（保证老用户的 config.toml 不会被打乱）。
fn apply_default_paths(cfg: &mut Config) {
    // v3.x：先尝试 portable 模式（launcher-portable.dat 存在时）
    match crate::paths::env_paths::resolve() {
        Ok(resolved) => {
            tracing::info!(
                "[env_paths] portable mode active: env_root={} env_name={} port={}",
                resolved.env_root.display(),
                resolved.env_name,
                resolved.port
            );
            tracing::info!(
                "[env_paths] comfyui={} venv={} custom_nodes={} (in_comfyui={})",
                resolved.comfyui_root.display(),
                resolved.venv_path.display(),
                resolved.custom_nodes.display(),
                resolved.custom_nodes_in_comfyui
            );

            // comfyui_root 必填（绿色版用 resolved 覆盖）
            if cfg.paths.comfyui_root.as_os_str().is_empty() {
                cfg.paths.comfyui_root = resolved.comfyui_root.clone();
            }
            // venv 必填
            if cfg.paths.venv_path.as_os_str().is_empty() {
                cfg.paths.venv_path = resolved.venv_path.clone();
            }
            // v3.x：custom_nodes 路径
            if cfg.paths.custom_nodes_path.is_none() {
                cfg.paths.custom_nodes_path = Some(resolved.custom_nodes.clone());
            }
            // v3.x：ComfyUI base_directory
            if cfg.paths.comfyui_base_directory.is_none() && resolved.override_base_directory {
                // custom_nodes 在 ComfyUI 外 → 传 custom_nodes 父目录作为 base
                let base = resolved.custom_nodes.parent()
                    .unwrap_or(&resolved.comfyui_root)
                    .to_path_buf();
                cfg.paths.comfyui_base_directory = Some(base);
            }
            // v3.x：环境名
            if cfg.env_name.is_none() {
                cfg.env_name = Some(resolved.env_name.clone());
            }
            // v3.x：端口（来自 portable.dat）
            // **v3.x 关键修复**：总是用 resolved.port 覆盖 cfg.launch.listen_port
            // - 旧版判断 `if resolved.port != 8188` → 复制的目录如果 port=8188 不会更新，导致端口冲突
            // - 现在 portable 模式下 listen_port 总是按目录级配置，**不保留**用户自定义
            // - 传统模式（无 launcher-portable.dat）走 fallback 分支，**保留**用户自定义
            // - 副作用：绿色版用户改 listen_port 不会持久化（这是"目录级配置"约定的代价）
            cfg.launch.listen_port = resolved.port;
            tracing::info!(
                "[env_paths] portable mode: listen_port overridden to {} (from launcher-portable.dat)",
                resolved.port
            );
            return;
        }
        Err(e) => {
            tracing::debug!(
                error = %e,
                "env_paths::resolve 失败，回退到 v1.8 默认逻辑"
            );
        }
    }

    // 关键：comfyui_root 必须是 portable_base_dir 的**子目录**（如 "ComfyUI"），
    // 不能直接等于 portable_base_dir 本身。原因：
    //   - portable_base_dir 通常已包含 launcher 自己的 .git、node_modules、src/ 等
    //   - 若直接用 portable_base_dir，CoreManager::is_cloned() 会返回 false（没有 ComfyUI 标记）
    //   - clone_repo 会检测到目录非空但无 .git → 抛 NotEmptyDir
    // 设为子目录后：目录不存在 → clone_repo 走"目录不存在"分支，正常 clone
    //
    // v1.8 / F38：使用 portable_base_dir 替代 launcher_working_dir
    //   - dev 模式：项目根（避免数据跑到 target/debug/）
    //   - prod 模式：exe 所在目录
    if cfg.paths.comfyui_root.as_os_str().is_empty() {
        cfg.paths.comfyui_root = paths::default_comfyui_root();
    }
    // v3.1 / F26 决策 1：venv 独立于 ComfyUI 仓库
    //   - 旧版默认 `<comfyui_root>/venv` 切版本时会被 git 操作影响
    //   - v1.8 起默认 `<app_data_dir>/venv`（即 `<portable_base>/data/venv`），
    //     跨版本切换 ComfyUI 不影响 venv，且和 launcher 一起走 portable 模式
    //   - 用户已配置的路径保留不动（向后兼容）
    if cfg.paths.venv_path.as_os_str().is_empty() {
        cfg.paths.venv_path = paths::app_data_dir().join("venv");
    }
    // v3.x：传统模式下默认 custom_nodes = `<comfyui_root>/custom_nodes`
    if cfg.paths.custom_nodes_path.is_none() {
        cfg.paths.custom_nodes_path = Some(cfg.paths.comfyui_root.join("custom_nodes"));
    }
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
    // **v1.8 / F36**：venv 路径不能在 src-tauri/ 子目录下
    // 原因：Tauri dev 自动监视 src-tauri/ 触发 rebuild，破坏 venv 安装长任务
    validate_venv_path_not_under_src_tauri(&cfg.paths.venv_path)?;
    Ok(())
}

/// **v3.x Phase 1**：把智能推荐结果应用到 cfg
///
/// 覆盖 `cfg.torch.cuda_version`（仅 NVIDIA 分支有意义）。
///
/// 设计：
/// - 只覆盖 NVIDIA CudaVersion（Cpu / AmdRocm / IntelXpu / AppleSilicon 在本项目暂不支持）
/// - torch_variant 字段保持 None（用户不主动调智能推荐按钮就不写）
/// - 不修改其他字段（保留用户可能改过的其他配置）
fn apply_recommended_torch(cfg: &mut Config, recommended: &TorchVariant) {
    if let TorchVariant::NvidiaCuda(cuda) = recommended {
        let mapped = match cuda {
            crate::python_env::torch_variant::CudaVersion::V11_8 => CudaVersion::Cu118,
            crate::python_env::torch_variant::CudaVersion::V12_6 => CudaVersion::Cu126,
            crate::python_env::torch_variant::CudaVersion::V12_8 => CudaVersion::Cu128,
            crate::python_env::torch_variant::CudaVersion::V13_0 => CudaVersion::Cu130,
        };
        tracing::info!(
            from = ?cfg.torch.cuda_version,
            to = ?mapped,
            "v3.x Phase 1: 首次启动用智能推荐覆盖默认 cuda_version"
        );
        cfg.torch.cuda_version = mapped;
    }
    // 非 NVIDIA 暂不处理（CPU/AMD/Intel/Apple 由各自模块处理）
}

/// **v1.8 / F36**：校验 venv 路径不在 src-tauri/ 子目录下
///
/// 场景：早期版本默认把 venv 放在 `src-tauri/venv/`，Tauri dev file watcher 会
/// 监视 src-tauri/ 下所有文件变化触发 rebuild，导致 uv pip install 改 venv 时
/// 整个启动器被重启，torch 安装永远装不全。
///
/// 修复：拒绝 venv 在 src-tauri/ 下的配置，让用户改到独立位置（如 app_data_dir/data/venv）。
pub fn validate_venv_path_not_under_src_tauri(venv_path: &std::path::Path) -> Result<(), AppError> {
    if venv_path.as_os_str().is_empty() {
        return Ok(()); // 空路径在 build_default_config 阶段填充，这里不阻塞
    }
    // 通过 CARGO_MANIFEST_DIR 环境变量（编译时）拿 src-tauri 的绝对路径
    // 也可以运行时通过当前可执行文件回溯：env!("CARGO_MANIFEST_DIR")
    let src_tauri_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let venv_abs = match std::fs::canonicalize(venv_path) {
        Ok(p) => p,
        Err(_) => {
            // 路径不存在（首次配置）→ 不阻塞，等创建后再校验
            return Ok(());
        }
    };
    if venv_abs.starts_with(src_tauri_dir) {
        return Err(ConfigError::InvalidValue {
            field: "paths.venv_path".into(),
            value: format!(
                "{}（在 src-tauri/ 下，会被 Tauri dev 监视触发启动器重启）",
                venv_path.display()
            ),
        }
        .into());
    }
    Ok(())
}

/// **v1.8 / F36**：Tauri 命令——前端调用获取 venv 路径是否合法
///
/// 用途：启动时 + 设置页保存 venv 路径后调用
/// 返回：None = 合法；Some(reason) = 不合法原因
#[tauri::command]
pub async fn config_validate_venv_path(
    state: tauri::State<'_, crate::app_state::AppState>,
) -> Result<Option<String>, String> {
    let venv_path = state.config.get().paths.venv_path.clone();
    match validate_venv_path_not_under_src_tauri(&venv_path) {
        Ok(()) => Ok(None),
        Err(e) => Ok(Some(e.to_string())),
    }
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

/// 把原始 TOML 文本原子写回磁盘（不做序列化重整）
///
/// 用于 config 预处理后写回：保持原有顺序、注释、空行，不引入 toml 序列化差异。
async fn save_raw_to_disk(path: &std::path::Path, content: &str) -> Result<(), ConfigError> {
    use crate::config::atomic_write;
    atomic_write::atomic_write(path, content)
        .await
        .map_err(|e| ConfigError::IoError(e.to_string()))
}

/// **v3.4.1**：预处理 TOML 文本，把旧版 `preview_method` 值替换为 ComfyUI 实际支持的新值
///
/// ## 背景
/// - 旧 `PreviewMethod` 枚举：`Latent`("latent") / `LatentUpscale`("latent-upscale") /
///   `Autoencoder`("autoencoder") / `None`("none")
/// - 新 `PreviewMethod` 枚举（与 ComfyUI main.py argparse 对齐）：`None`("none") /
///   `Auto`("auto") / `Latent2Rgb`("latent2rgb") / `Taesd`("taesd")
/// - 旧值直接传 `--preview-method` 会让 main.py argparse 失败 → 进程退出码 2
///
/// ## 为什么不用 serde 兼容层
/// - 直接改 `Deserialize` 实现增加 legacy 别名可行，但会让序列化输出非标准值
/// - 在解析前替换文本更简单，且不污染运行时数据
///
/// ## 实现细节
/// - 文本替换：仅处理 `preview_method = "..."` 这一行，**不**全局替换 `"latent"` 字符串
///   （避免误伤其他无关字段）
/// - 只在文件内确实出现旧值时才替换；命中旧值时返回 `(from, to)` 供调用方打日志
/// - 不动 schema_version（preview_method 字符串迁移是**兼容性补丁**，不是 schema 升级）
///
/// ## 旧 → 新映射
/// - `latent` → `latent2rgb`
/// - `latent-upscale` → `taesd`
/// - `autoencoder` → `auto`
/// - 其他未知值 → `latent2rgb`（保守默认值）
fn preprocess_legacy_toml(raw: &str) -> (String, Option<(String, String)>) {
    use super::models::migrate_legacy_preview_method;

    let mut out = String::with_capacity(raw.len());
    let mut migrated: Option<(String, String)> = None;

    for line in raw.lines() {
        let trimmed = line.trim_start();
        // 匹配 `preview_method = "..."`（不区分前后空白，但保留原缩进）
        if trimmed.starts_with("preview_method") && trimmed.contains('=') {
            // 拆 "preview_method" "=" 值
            let parts: Vec<&str> = trimmed.splitn(2, '=').collect();
            if parts.len() == 2 {
                let value_part = parts[1].trim();
                // 提取字符串字面量
                let stripped = value_part
                    .trim_start_matches('"')
                    .trim_end_matches('"')
                    .trim_end_matches('\'')
                    .trim_start_matches('\'');
                if let Some(new_val) = migrate_legacy_preview_method(stripped) {
                    if migrated.is_none() && stripped != new_val {
                        migrated = Some((stripped.to_string(), new_val.clone()));
                    }
                    // 重建这一行：保留前缀缩进 + key + = + 新的带引号值
                    let indent = &line[..line.len() - trimmed.len()];
                    // 原始 value 部分的引号风格
                    let quote = if value_part.contains('"') { '"' } else { '\'' };
                    let new_line = format!(
                        "{}{} = {}{}{}",
                        indent,
                        "preview_method",
                        quote,
                        new_val,
                        quote
                    );
                    out.push_str(&new_line);
                    out.push('\n');
                    continue;
                }
            }
        }
        out.push_str(line);
        out.push('\n');
    }

    // 去掉最后多塞的 \n（如果原文以 \n 结尾会多一个）
    if raw.ends_with('\n') && out.ends_with('\n') && out.len() > raw.len() {
        out.pop();
    }

    (out, migrated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_event_bus() -> EventBus {
        EventBus::new()
    }

    /// **v3.x 绿色版**：load() 不创建 config.toml（等首次 update 时才创建）
    #[tokio::test]
    async fn test_load_default_when_absent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let svc = ConfigService::load(path.clone(), test_event_bus()).await.unwrap();

        assert_eq!(svc.config_path(), path);
        // v3.x：首次 load **不**创建文件（让"看上去没配置过"成为默认）
        assert!(!path.exists(), "v3.x: 首次启动不应自动创建 config.toml");
        // 内存中应该有默认配置
        assert_eq!(svc.get().launch.listen_port, 8188);
    }

    /// **v3.x**：第一次 update 才创建 config.toml
    #[tokio::test]
    async fn test_load_creates_config_on_first_update() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let svc = ConfigService::load(path.clone(), test_event_bus()).await.unwrap();

        // load 不创建
        assert!(!path.exists());

        // update 后才创建
        svc.update(|cfg| {
            cfg.launch.listen_port = 9999;
            Ok(())
        })
        .await
        .unwrap();

        assert!(path.exists(), "v3.x: 首次 update 后应创建 config.toml");

        // **v3.x 绿色版关键断言**：config.toml 里**不**含 [paths] 段
        // v3.x 修复：用 ConfigForToml 视图结构，根本不带 paths 段
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("[paths]"),
            "v3.x: config.toml 不应包含 [paths] 段（ConfigForToml 视图结构强制跳过）"
        );

        // **v3.x 关键修复**：
        // invoke 序列化（给前端）走完整 Config，必须能拿到 paths
        // 之前用 skip_serializing 把 invoke 序列化也跳了 → 前端拿不到 comfyui_root
        // 现在 PathsConfig 字段没有 skip_serializing，invoke 应该返回完整路径
        let cfg = svc.get();
        assert!(
            !cfg.paths.comfyui_root.as_os_str().is_empty()
                || cfg.paths.comfyui_root.as_os_str().is_empty(),
            "v3.x: invoke 序列化能返回 comfyui_root（不管值）"
        );
        // 关键：serde_json 序列化能正确输出 paths 段
        let json = serde_json::to_string(&cfg.paths).unwrap();
        assert!(
            json.contains("comfyui_root") || json.contains("\"comfyui_root\""),
            "v3.x 修复: serde_json 序列化必须包含 comfyui_root（之前被 skip_serializing 跳了）"
        );
    }

    /// **v3.x 关键修复**：invoke 序列化（serde_json）必须返回完整 PathsConfig
    ///
    /// 这是修复前路由守卫 bug 的关键测试：
    /// - 旧版：PathsConfig 字段 `#[serde(skip_serializing)]` 同时影响 serde_json
    ///   → 前端 `cfg.paths.comfyui_root` 永远是空
    ///   → 路由守卫 `!configStore.comfyuiRoot === true` → 把用户弹回 /onboarding
    /// - 修复：移除字段级 skip_serializing + 用 ConfigForToml 视图结构控制 TOML 路径
    #[tokio::test]
    async fn test_invoke_serialization_includes_paths() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let svc = ConfigService::load(path.clone(), test_event_bus()).await.unwrap();

        // 模拟"用户填了 comfyui_root"（在内存 cfg 中）
        svc.update(|cfg| {
            cfg.paths.comfyui_root = PathBuf::from("D:\\AIWork\\myComfyui\\ComfyUI");
            cfg.paths.venv_path = PathBuf::from("D:\\AIWork\\myComfyui\\data\\venv");
            cfg.paths.python_version = "3.11".to_string();
            Ok(())
        })
        .await
        .unwrap();

        // **关键断言 1**：serde_json 序列化（给前端）必须包含 comfyui_root
        let cfg = &**svc.get();
        let json = serde_json::to_string(cfg).unwrap();
        assert!(
            json.contains("D:\\\\AIWork\\\\myComfyui\\\\ComfyUI"),
            "v3.x 修复: serde_json 必须包含 comfyui_root 完整值（之前是空字符串）"
        );
        assert!(
            json.contains("D:\\\\AIWork\\\\myComfyui\\\\data\\\\venv"),
            "v3.x 修复: serde_json 必须包含 venv_path 完整值"
        );
        assert!(
            json.contains("3.11"),
            "v3.x 修复: serde_json 必须包含 python_version"
        );

        // **关键断言 2**：TOML 持久化（写 config.toml）**不**包含 paths 段
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("[paths]"),
            "v3.x: config.toml 不应包含 [paths] 段（绿色版约定：复制目录时路径自动适配）"
        );
        assert!(
            !content.contains("D:\\\\AIWork\\\\myComfyui\\\\ComfyUI"),
            "v3.x: 绝对路径不应写入 config.toml"
        );

        println!("\n[TOML 内容]:\n{}\n", content);
        println!("[JSON 路径段]:\n{}\n",
            serde_json::to_string_pretty(&cfg.paths).unwrap());
    }

    #[tokio::test]
    async fn test_save_load_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let svc = ConfigService::load(path.clone(), test_event_bus()).await.unwrap();
        svc.update(|cfg| {
            cfg.launch.listen_port = 9999;
            cfg.torch.cuda_version = CudaVersion::Cu128;
            Ok(())
        }).await.unwrap();

        // 重新加载验证
        let svc2 = ConfigService::load(path, test_event_bus()).await.unwrap();
        assert_eq!(svc2.get().launch.listen_port, 9999);
        assert_eq!(svc2.get().torch.cuda_version, CudaVersion::Cu128);
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

    // === v3.4.1: preview_method 旧值迁移测试 ===

    #[test]
    fn test_preprocess_legacy_toml_latent_to_latent2rgb() {
        let raw = r#"
[launch]
mode = "gpu_high"
preview_method = "latent"
listen_port = 8188
"#;
        let (out, migrated) = preprocess_legacy_toml(raw);
        assert_eq!(migrated, Some(("latent".into(), "latent2rgb".into())));
        assert!(out.contains(r#"preview_method = "latent2rgb""#));
        assert!(!out.contains(r#"preview_method = "latent""#));
    }

    #[test]
    fn test_preprocess_legacy_toml_latent_upscale() {
        let raw = r#"preview_method = "latent-upscale"
"#;
        let (out, migrated) = preprocess_legacy_toml(raw);
        assert_eq!(migrated, Some(("latent-upscale".into(), "taesd".into())));
        assert!(out.contains(r#"preview_method = "taesd""#));
    }

    #[test]
    fn test_preprocess_legacy_toml_autoencoder() {
        let raw = r#"preview_method = "autoencoder"
"#;
        let (out, migrated) = preprocess_legacy_toml(raw);
        assert_eq!(migrated, Some(("autoencoder".into(), "auto".into())));
        assert!(out.contains(r#"preview_method = "auto""#));
    }

    #[test]
    fn test_preprocess_legacy_toml_new_value_unchanged() {
        let raw = r#"preview_method = "latent2rgb"
"#;
        let (out, migrated) = preprocess_legacy_toml(raw);
        assert_eq!(migrated, None, "新值不应触发迁移");
        assert!(out.contains(r#"preview_method = "latent2rgb""#));
    }

    #[test]
    fn test_preprocess_legacy_toml_unknown_value_to_latent2rgb() {
        let raw = r#"preview_method = "garbage_value"
"#;
        let (out, migrated) = preprocess_legacy_toml(raw);
        assert_eq!(migrated, Some(("garbage_value".into(), "latent2rgb".into())));
        assert!(out.contains(r#"preview_method = "latent2rgb""#));
    }

    #[test]
    fn test_preprocess_legacy_toml_no_preview_method_field() {
        let raw = r#"
[launch]
mode = "gpu_high"
listen_port = 8188
"#;
        let (out, migrated) = preprocess_legacy_toml(raw);
        assert_eq!(migrated, None);
        assert_eq!(out.trim(), raw.trim());
    }

    #[test]
    fn test_preprocess_legacy_toml_preserves_indent() {
        let raw = "preview_method = \"latent\"\n";
        let (out, _) = preprocess_legacy_toml(raw);
        // 没有缩进，应当保持无缩进
        assert!(out.starts_with(r#"preview_method = "latent2rgb""#));
    }

    #[test]
    fn test_preprocess_legacy_toml_preserves_indent_with_tab() {
        let raw = "\tpreview_method = \"latent\"\n";
        let (out, _) = preprocess_legacy_toml(raw);
        // tab 缩进应当保留
        assert!(out.starts_with("\tpreview_method = \"latent2rgb\""));
    }
}
