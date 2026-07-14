//! **统一路径解析**（v0.0.2 重构，v0.0.2.1 进一步收紧）
//!
//! 之前有两套并存的路径系统（`common/paths.rs` + 本模块），dev 模式下解析出的根
//! 不同（项目根 vs `target/debug/`），导致 UI 显示不一致。本模块把两套合并为
//! **唯一** 的路径解析入口。
//!
//! ## 核心原则
//!
//! - **唯一根**：所有路径相对 `<exe_dir>` 解析（`current_exe().parent()`）
//! - **dev / prod 一致**：`tauri dev` 编译到 `target/debug/`，prod 编译到 `target/release/`
//!   → 两条路径都解析为各自的 `<exe_dir>`，**不**做"dev 用项目根"的特殊化
//! - **多实例隔离**：复制整个目录到新位置 → 新实例自动用新 `<exe_dir>` → 互不影响 ✅
//! - **缓存紧贴 exe**：所有用户可见目录（`data/`、`cache/`、`ComfyUI/`）都在 `<exe_dir>` 旁边
//!   → 复制目录就等于整体复制一份绿色版环境
//!
//! ## 目录布局
//!
//! ```text
//! <exe_dir>/                              ← launcher.exe 所在目录
//! ├── BoundLaunch.exe / .dll              # launcher 自身
//! ├── launcher-portable.dat               # 配置（v3.x）
//! ├── resources/uv/                       # 打包时自带（v3.x）
//! ├── ComfyUI/                            # ComfyUI 核心（onboarding 时拉取）
//! │   ├── main.py
//! │   ├── models/                         # 默认模型目录
//! │   └── custom_nodes/                   # 默认插件目录
//! ├── data/                               # 用户持久数据（可见）
//! │   ├── venv/                           # Python venv
//! │   │   ├── pyvenv.cfg
//! │   │   └── Lib/...
//! │   └── uv/                             # uv sidecar 部署
//! │       └── uv.exe
//! ├── cache/                              # 用户缓存（可见）
//! │   └── transformers_versions.json      # PyPI transformers 版本索引
//! └── .boundlaunch/                       # launcher 私有数据（隐藏）
//!     ├── launcher.db                     # SQLite（日志 / 任务历史）
//!     ├── config.toml                     # 用户配置
//!     ├── launcher.pid                    # launcher PID（崩溃恢复）
//!     ├── logs/                           # launcher 日志
//!     │   ├── launcher.log
//!     │   └── launch-2026-07-13.log
//!     └── sessions/                       # ComfyUI session（多实例隔离）
//!         ├── 1234567890-abc.session
//!         └── ...
//! ```
//!
//! ## 配置：launcher-portable.dat
//!
//! 所有相对路径都在这个文件里配置（详见 `PortableConfig` / `PortablePaths`）。
//! 找不到 → 自动写默认配置（首次启动）。
//!
//! ## 多实例隔离
//!
//! - **路径层**：每个实例 `<exe_dir>` 独立，session 文件路径自然隔离
//! - **端口层**：每个实例的 `port` 字段独立，避免冲突
//! - **数据库层**：每个实例的 `.boundlaunch/launcher.db` 独立
//!
//! ## custom_nodes 路径机制（关键！）
//!
//! ComfyUI 的 `folder_paths.py:41` 决定 custom_nodes 默认位置：
//! ```python
//! folder_names_and_paths["custom_nodes"] = ([os.path.join(base_path, "custom_nodes")], set())
//! ```
//! `base_path` 默认 = `os.path.dirname(os.path.realpath(__file__))` = ComfyUI 目录
//!
//! 如果用户把 custom_nodes 放到 ComfyUI 外：
//! - launcher 启动 ComfyUI 时加 `--base-directory <custom_nodes 父目录>`
//! - ComfyUI 的 base_path 变成这个新目录
//! - custom_nodes 自动从 `<新目录>/custom_nodes/` 找
//!
//! 详见 `PR/03-模块设计/04-PluginManager.md §custom_nodes 路径机制`
//!
//! ## 历史
//!
//! - **v1.8 / F38**：引入 `common/paths.rs`，dev 模式用 `CARGO_MANIFEST_DIR` 父目录
//! - **v3.x**：引入 `env_paths.rs`，绿色版用 `current_exe().parent()`
//! - **v0.0.2**：两套系统合并，统一用 `current_exe().parent()`，dev/prod 行为一致
//! - **v0.0.2.1**：彻底删除 `BOUND_LAUNCH_DATA_DIR` 兼容分支、删除 `maybe_migrate_to_portable`、
//!   删掉 `portable_base_dir` / `legacy_data_dir` / `launcher_working_dir` 等所有旧 API；
//!   缓存目录固定在 `<exe_dir>/cache/`

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// portable.dat 文件名（必须和 BoundLaunch.exe 在同一目录）
pub const PORTABLE_CONFIG_FILENAME: &str = "launcher-portable.dat";

/// launcher 私有数据目录（隐藏在 `<exe_dir>` 下）
pub const BOUNDLAUNCH_DATA_SUBDIR: &str = ".boundlaunch";

/// 用户持久数据目录（可见）
pub const DATA_SUBDIR: &str = "data";

/// 用户缓存目录（可见）
pub const CACHE_SUBDIR: &str = "cache";

/// ComfyUI 子目录名
pub const COMFYUI_SUBDIR: &str = "ComfyUI";

/// 默认端口（解析失败时兜底用）
pub const DEFAULT_PORT: u16 = 8188;

// =============================================================================
// portable.dat 数据结构
// =============================================================================

/// launcher-portable.dat 数据结构
///
/// **TOML 格式**：
/// ```toml
/// version = 1
/// name = "EnvA-SD15"
/// port = 8188
/// override_base_directory = false
///
/// [paths]
/// comfyui = "ComfyUI"
/// venv = "data/venv"
/// custom_nodes = "ComfyUI/custom_nodes"
/// models = "ComfyUI/models"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PortableConfig {
    /// 配置文件格式版本
    #[serde(default = "default_version")]
    pub version: u32,

    /// 环境名（用于日志、UI 显示、数据库命名空间）
    ///
    /// **默认**：取自 exe 所在目录的目录名
    /// **规则**：空字符串 = 用目录名兜底
    #[serde(default)]
    pub name: String,

    /// ComfyUI 启动端口（避免多环境端口冲突）
    ///
    /// **默认**：8188
    /// **冲突时**：启动器自动检测空闲端口（8188 → 8189 → ...）
    #[serde(default = "default_port")]
    pub port: u16,

    /// 是否覆盖 ComfyUI 的 base_directory
    ///
    /// **规则**：
    /// - `false`（默认）：不传 `--base-directory`，ComfyUI 用自己的 folder_paths.py
    ///   → custom_nodes / models / input / output 都在 <ComfyUI>/ 下
    /// - `true`：传 `--base-directory <custom_nodes 父目录>`
    ///   → custom_nodes 可以放到 ComfyUI 外
    ///
    /// **自动推断**：当 `paths.custom_nodes` 在 comfyui 外时，本字段自动设为 true
    #[serde(default)]
    pub override_base_directory: bool,

    /// 路径配置
    #[serde(default)]
    pub paths: PortablePaths,
}

fn default_version() -> u32 { 1 }
fn default_port() -> u16 { DEFAULT_PORT }

/// 路径配置（相对 <exe_dir>）
///
/// **v0.0.2**：移除 `boundlaunch_data` 字段——`.boundlaunch/` 固定写死，
/// 不允许配置（避免误把数据库/日志放到用户可见目录）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PortablePaths {
    /// ComfyUI 核心目录
    #[serde(default = "default_comfyui")]
    pub comfyui: String,

    /// Python venv 目录
    #[serde(default = "default_venv")]
    pub venv: String,

    /// custom_nodes 目录
    ///
    /// **规则**：
    /// - 相对路径 → 相对 <exe_dir>（如 `"ComfyUI/custom_nodes"`）
    /// - 绝对路径 → 直接用（如 `"D:/SharedCustomNodes"`）
    /// - **默认**：`"ComfyUI/custom_nodes"`（在 ComfyUI 内）
    #[serde(default = "default_custom_nodes")]
    pub custom_nodes: String,

    /// 模型目录
    #[serde(default = "default_models")]
    pub models: String,
}

fn default_comfyui() -> String { COMFYUI_SUBDIR.to_string() }
fn default_venv() -> String { format!("{}/venv", DATA_SUBDIR) }
fn default_custom_nodes() -> String { format!("{}/custom_nodes", COMFYUI_SUBDIR) }
fn default_models() -> String { format!("{}/models", COMFYUI_SUBDIR) }

impl Default for PortablePaths {
    fn default() -> Self {
        Self {
            comfyui: default_comfyui(),
            venv: default_venv(),
            custom_nodes: default_custom_nodes(),
            models: default_models(),
        }
    }
}

impl Default for PortableConfig {
    fn default() -> Self {
        Self {
            version: default_version(),
            name: String::new(),
            port: default_port(),
            override_base_directory: false,
            paths: PortablePaths::default(),
        }
    }
}

// =============================================================================
// 解析后的路径（绝对路径）
// =============================================================================

/// 解析后的环境路径（绝对路径）
///
/// **v0.0.2**：这是 launcher 启动时唯一应该使用的路径集合。
/// 所有路径都是绝对路径，全部以 `<exe_dir>` 为根。
#[derive(Debug, Clone)]
pub struct ResolvedEnvPaths {
    // ===== 根标识 =====

    /// 环境根目录（= `current_exe().parent()`，绝对路径）
    ///
    /// 复制目录到新位置 → 新实例 env_root 自动指向新位置 → 多实例完全隔离
    pub env_root: PathBuf,

    /// 环境名（来自 portable.dat `name`，空则用 env_root 的目录名）
    pub env_name: String,

    /// ComfyUI 启动端口（来自 portable.dat `port`，解析失败时 = DEFAULT_PORT）
    pub port: u16,

    // ===== 用户可见目录（绿色版结构） =====

    /// ComfyUI 核心目录（= `<env_root>/<cfg.paths.comfyui>`）
    pub comfyui_root: PathBuf,

    /// venv 绝对路径（= `<env_root>/<cfg.paths.venv>`）
    pub venv_path: PathBuf,

    /// custom_nodes 绝对路径（= `<env_root>/<cfg.paths.custom_nodes>`）
    pub custom_nodes: PathBuf,

    /// custom_nodes 是否在 ComfyUI 内
    pub custom_nodes_in_comfyui: bool,

    /// 模型目录（= `<env_root>/<cfg.paths.models>`）
    pub models_dir: PathBuf,

    /// ComfyUI 启动时是否要传 `--base-directory`
    pub override_base_directory: bool,

    // ===== 用户持久数据目录（`<env_root>/data/`，可见） =====

    /// 用户数据目录（= `<env_root>/data/`）
    pub app_data_dir: PathBuf,

    /// uv sidecar 部署目录（= `<app_data_dir>/uv/`）
    pub uv_deploy_dir: PathBuf,

    /// uv sidecar 二进制路径（= `<uv_deploy_dir>/uv[.exe]`）
    pub uv_binary_path: PathBuf,

    // ===== 用户缓存目录（`<env_root>/cache/`，可见） =====

    /// 用户缓存目录（= `<env_root>/cache/`）
    pub cache_dir: PathBuf,

    /// transformers 版本缓存文件
    pub transformers_cache_path: PathBuf,

    // ===== launcher 私有数据（`<env_root>/.boundlaunch/`，隐藏） =====

    /// launcher 私有数据目录（= `<env_root>/.boundlaunch/`）
    pub boundlaunch_data_dir: PathBuf,

    /// 用户配置（= `<boundlaunch_data_dir>/config.toml`）
    pub config_path: PathBuf,

    /// SQLite 数据库（= `<boundlaunch_data_dir>/launcher.db`）
    pub database_path: PathBuf,

    /// launcher PID 文件（崩溃恢复用）
    pub pid_file_path: PathBuf,

    /// launcher 日志目录
    pub logs_dir: PathBuf,

    /// ComfyUI session 目录（多实例隔离）
    pub sessions_dir: PathBuf,

    // ===== portable 配置 =====

    /// portable.dat 路径
    pub portable_config_path: PathBuf,
}

#[derive(Debug, Error)]
pub enum EnvPathsError {
    #[error("无法获取当前 exe 路径: {0}")]
    ExePathNotFound(String),

    #[error("环境根目录不存在: {0}")]
    EnvRootNotFound(PathBuf),

    #[error("解析 portable.dat 失败: {0}")]
    ParseError(String),

    #[error("写入 portable.dat 失败: {0}")]
    WriteError(String),
}

// =============================================================================
// 核心 API
// =============================================================================

/// 找到 BoundLaunch.exe 所在目录
///
/// **v0.0.2**：统一用 `current_exe().parent()`（不再区分 dev/prod）
///
/// **历史**：v1.8 / F38 dev 模式用 `CARGO_MANIFEST_DIR` 的父目录，
/// prod 模式用 `current_exe().parent()`。v0.0.2 合并为唯一行为。
pub fn find_exe_dir() -> Result<PathBuf, EnvPathsError> {
    let exe = std::env::current_exe()
        .map_err(|e| EnvPathsError::ExePathNotFound(e.to_string()))?;
    let dir = exe
        .parent()
        .ok_or_else(|| EnvPathsError::ExePathNotFound("no parent".to_string()))?
        .to_path_buf();
    Ok(dir)
}

/// portable.dat 路径（不一定存在）
pub fn portable_config_path() -> Result<PathBuf, EnvPathsError> {
    let dir = find_exe_dir()?;
    Ok(dir.join(PORTABLE_CONFIG_FILENAME))
}

/// 读 portable 配置
///
/// **设计**：
/// - 不存在 → 写默认配置并返回（首次启动）
/// - 存在 → 解析
/// - 解析失败 → 返回错误（不自动修复，避免掩盖用户配置问题）
///
/// **v0.0.2.1**：解析后**强制归一化** `cfg.paths.*` 为平台原生分隔符，
/// 并立即写回磁盘修正可能存在的 mixed-separator 历史数据。
/// 理由：portable.dat 是跨平台约定（用 `/`），但 Windows API 走 `\`，
/// 不归一化会导致 `env_paths::resolve()` 算出 `D:\debug\data/venv` 这种
/// mixed 路径，UI 显示与真实文件位置不一致。
pub fn load_or_create() -> Result<(PortableConfig, PathBuf), EnvPathsError> {
    let path = portable_config_path()?;
    if path.exists() {
        let content = std::fs::read_to_string(&path)
            .map_err(|e| EnvPathsError::ParseError(e.to_string()))?;
        let mut cfg: PortableConfig = toml::from_str(&content)
            .map_err(|e| EnvPathsError::ParseError(e.to_string()))?;

        // 归一化 relative 路径字段
        let before = (
            cfg.paths.comfyui.clone(),
            cfg.paths.venv.clone(),
            cfg.paths.custom_nodes.clone(),
            cfg.paths.models.clone(),
        );
        normalize_portable_paths(&mut cfg);
        let after = (
            &cfg.paths.comfyui,
            &cfg.paths.venv,
            &cfg.paths.custom_nodes,
            &cfg.paths.models,
        );
        if before.0 != *after.0 || before.1 != *after.1 || before.2 != *after.2 || before.3 != *after.3 {
            // 修正了历史数据 → 立即写回
            let new_content = toml::to_string_pretty(&cfg)
                .map_err(|e| EnvPathsError::WriteError(e.to_string()))?;
            std::fs::write(&path, new_content)
                .map_err(|e| EnvPathsError::WriteError(e.to_string()))?;
            tracing::info!(
                "[env_paths] 修正 portable.dat 里的 mixed-separator 路径: {}",
                path.display()
            );
        }

        Ok((cfg, path))
    } else {
        // 首次启动：写默认配置
        let dir = find_exe_dir()?;
        let mut cfg = PortableConfig::default();
        if cfg.name.is_empty() {
            cfg.name = dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("default")
                .to_string();
        }
        // 归一化默认路径为 native separator（写盘后下次读也是 native）
        normalize_portable_paths(&mut cfg);
        let content = toml::to_string_pretty(&cfg)
            .map_err(|e| EnvPathsError::WriteError(e.to_string()))?;
        std::fs::write(&path, content)
            .map_err(|e| EnvPathsError::WriteError(e.to_string()))?;
        tracing::info!(
            "[env_paths] 首次启动，已写默认配置: {}",
            path.display()
        );
        Ok((cfg, path))
    }
}

/// 把相对路径解析为绝对路径
///
/// **v0.0.2.1 修复**：在 Windows 上，把 child 里的所有 `/` 替换为 `\`
/// 避免 portable.dat 写的 `data/venv`（TOML 跨平台约定用正斜杠）跟
/// `env_root`（Windows API 用反斜杠）join 出 `D:\debug\data/venv` 这种
/// mixed separator 路径，UI 显示不一致、用户复制到资源管理器打不开。
///
/// **规则**：
/// - 绝对路径 → 原样返回
/// - 相对路径 → 相对 <base>
fn resolve_relative(base: &Path, rel: &str) -> PathBuf {
    // **平台原生分隔符归一化**：把 child 里所有"错方向"的斜杠替换为平台原生
    // 理由：portable.dat 跨平台约定用 `/`，但 Windows API 走 `\`，
    //       必须显式归一化才能 join 出纯 native 路径
    #[cfg(windows)]
    let rel_native = rel.replace('/', "\\");
    #[cfg(not(windows))]
    let rel_native = rel.replace('\\', "/");

    let p = Path::new(&rel_native);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

/// 归一化 portable.dat 里的相对路径字符串（按平台原生分隔符）
///
/// **v0.0.2.1**：在 `load_or_create` 末尾对 cfg.paths.* 调一次，
/// 保证内存里的 `cfg.paths.comfyui` 等字段都是 native separator。
/// 写盘时也用归一化后的值，旧 mixed 数据一次性修正。
fn normalize_portable_paths(cfg: &mut PortableConfig) {
    #[cfg(windows)]
    {
        cfg.paths.comfyui = cfg.paths.comfyui.replace('/', "\\");
        cfg.paths.venv = cfg.paths.venv.replace('/', "\\");
        cfg.paths.custom_nodes = cfg.paths.custom_nodes.replace('/', "\\");
        cfg.paths.models = cfg.paths.models.replace('/', "\\");
    }
    #[cfg(not(windows))]
    {
        cfg.paths.comfyui = cfg.paths.comfyui.replace('\\', "/");
        cfg.paths.venv = cfg.paths.venv.replace('\\', "/");
        cfg.paths.custom_nodes = cfg.paths.custom_nodes.replace('\\', "/");
        cfg.paths.models = cfg.paths.models.replace('\\', "/");
    }
}

/// 解析为完整路径集
///
/// **v0.0.2**：这是 launcher 启动时唯一应该调用的函数。
/// 返回的 `ResolvedEnvPaths` 包含所有路径，前端 / 后端都从这里取。
pub fn resolve() -> Result<ResolvedEnvPaths, EnvPathsError> {
    let (cfg, portable_path) = load_or_create()?;
    let env_root = find_exe_dir()?;
    if !env_root.exists() {
        return Err(EnvPathsError::EnvRootNotFound(env_root));
    }

    // ----- 解析用户可见目录 -----
    let comfyui_root = resolve_relative(&env_root, &cfg.paths.comfyui);
    let custom_nodes = resolve_relative(&env_root, &cfg.paths.custom_nodes);
    let custom_nodes_in_comfyui = is_under(&custom_nodes, &comfyui_root);

    // 自动推断 override_base_directory
    let override_base_directory = if cfg.override_base_directory {
        true
    } else {
        !custom_nodes_in_comfyui
    };

    // ----- 用户持久数据目录 -----
    // 固定在 `<env_root>/data/`。v0.0.2 彻底删除 `BOUND_LAUNCH_DATA_DIR` 兼容分支
    // （用户要求缓存目录全放 exe 旁边，APP DATA_DIR 兼容是历史包袱）。
    let app_data_dir = env_root.join(DATA_SUBDIR);

    // ----- 用户缓存目录 -----
    // 固定在 `<env_root>/cache/`，紧贴 exe。
    let cache_dir = env_root.join(CACHE_SUBDIR);

    // ----- launcher 私有数据（`.boundlaunch/`） -----
    // 固定在 `<env_root>/.boundlaunch/`，**不**支持任何覆盖
    let boundlaunch_data_dir = env_root.join(BOUNDLAUNCH_DATA_SUBDIR);

    // ----- 解析 env_name -----
    let env_name = if cfg.name.is_empty() {
        env_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("default")
            .to_string()
    } else {
        cfg.name
    };

    // ----- 派生子路径 -----
    let uv_deploy_dir = app_data_dir.join("uv");
    let uv_binary_name = if cfg!(windows) { "uv.exe" } else { "uv" };
    let uv_binary_path = uv_deploy_dir.join(uv_binary_name);

    let transformers_cache_path = cache_dir.join("transformers_versions.json");

    let database_path = boundlaunch_data_dir.join("launcher.db");
    let pid_file_path = boundlaunch_data_dir.join("launcher.pid");
    let logs_dir = boundlaunch_data_dir.join("logs");
    let sessions_dir = boundlaunch_data_dir.join("sessions");

    Ok(ResolvedEnvPaths {
        env_root: env_root.clone(),
        env_name,
        port: cfg.port,
        comfyui_root,
        venv_path: resolve_relative(&env_root, &cfg.paths.venv),
        custom_nodes,
        custom_nodes_in_comfyui,
        models_dir: resolve_relative(&env_root, &cfg.paths.models),
        override_base_directory,
        app_data_dir,
        uv_deploy_dir,
        uv_binary_path,
        cache_dir,
        transformers_cache_path,
        boundlaunch_data_dir: boundlaunch_data_dir.clone(),
        config_path: boundlaunch_data_dir.join("config.toml"),
        database_path,
        pid_file_path,
        logs_dir,
        sessions_dir,
        portable_config_path: portable_path,
    })
}

/// 判断 child 是否在 parent 下（路径前缀关系）
fn is_under(child: &Path, parent: &Path) -> bool {
    let child_canon = child.canonicalize().unwrap_or_else(|_| child.to_path_buf());
    let parent_canon = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
    child_canon.starts_with(&parent_canon)
}

// =============================================================================
// 工具函数
// =============================================================================

/// 确保目录存在（递归创建）
///
/// 替代 `common::paths::ensure_dir`（v0.0.2.1 已删除）
pub async fn ensure_dir(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        tokio::fs::create_dir_all(path).await?;
    }
    Ok(())
}

// =============================================================================
// 单元测试
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("env_paths_test_{}", name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_default_config() {
        let cfg = PortableConfig::default();
        assert_eq!(cfg.version, 1);
        assert_eq!(cfg.port, DEFAULT_PORT);
        assert_eq!(cfg.paths.comfyui, "ComfyUI");
        // v0.0.2.1：默认字段保留正斜杠（TOML 跨平台约定），归一化在 load_or_create 时再做
        assert_eq!(cfg.paths.venv, "data/venv");
        assert_eq!(cfg.paths.custom_nodes, "ComfyUI/custom_nodes");
        assert_eq!(cfg.paths.models, "ComfyUI/models");
        assert!(!cfg.override_base_directory);
    }

    /// v0.0.2.1：归一化后 portable.dat 路径字段全部变平台原生分隔符
    #[test]
    fn test_normalize_portable_paths_windows() {
        #[cfg(windows)]
        {
            let mut cfg = PortableConfig {
                version: 1,
                name: "test".into(),
                port: 8188,
                override_base_directory: false,
                paths: PortablePaths {
                    comfyui: "ComfyUI".into(),
                    venv: "data/venv".into(),
                    custom_nodes: "ComfyUI/custom_nodes".into(),
                    models: "ComfyUI/models".into(),
                },
            };
            normalize_portable_paths(&mut cfg);
            assert_eq!(cfg.paths.comfyui, "ComfyUI");
            assert_eq!(cfg.paths.venv, "data\\venv");
            assert_eq!(cfg.paths.custom_nodes, "ComfyUI\\custom_nodes");
            assert_eq!(cfg.paths.models, "ComfyUI\\models");
        }
        #[cfg(not(windows))]
        {
            // Unix 平台：保持正斜杠
            let mut cfg = PortableConfig {
                version: 1,
                name: "test".into(),
                port: 8188,
                override_base_directory: false,
                paths: PortablePaths {
                    comfyui: "ComfyUI".into(),
                    venv: "data\\venv".into(),  // Windows-style 写盘
                    custom_nodes: "ComfyUI\\custom_nodes".into(),
                    models: "ComfyUI\\models".into(),
                },
            };
            normalize_portable_paths(&mut cfg);
            assert_eq!(cfg.paths.venv, "data/venv");
        }
    }

    /// v0.0.2.1：resolve_relative 在 Windows 上把 child 里的 `/` 替换为 `\`
    /// 避免 join 出来 mixed separator
    #[test]
    fn test_resolve_relative_native_separator() {
        let base = PathBuf::from(if cfg!(windows) { r"D:\test\base" } else { "/test/base" });
        let resolved = resolve_relative(&base, "data/venv");
        if cfg!(windows) {
            // Windows：全部反斜杠
            assert_eq!(resolved, PathBuf::from(r"D:\test\base\data\venv"));
        } else {
            // Unix：全部正斜杠
            assert_eq!(resolved, PathBuf::from("/test/base/data/venv"));
        }
    }

    #[test]
    fn test_resolve_relative_path() {
        // Unix 平台的简单断言（Windows 上 child 会被转成反斜杠）
        #[cfg(not(windows))]
        {
            let base = PathBuf::from("/test/base");
            assert_eq!(
                resolve_relative(&base, "ComfyUI"),
                PathBuf::from("/test/base/ComfyUI")
            );
            assert_eq!(
                resolve_relative(&base, "/abs/path"),
                PathBuf::from("/abs/path")
            );
        }
        // Windows 平台独立测试在 test_resolve_relative_native_separator
    }

    #[test]
    fn test_serialize_roundtrip() {
        let cfg = PortableConfig::default();
        let s = toml::to_string_pretty(&cfg).unwrap();
        let back: PortableConfig = toml::from_str(&s).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn test_serialize_with_custom_values() {
        let cfg = PortableConfig {
            version: 1,
            name: "MyEnv".to_string(),
            port: 9999,
            override_base_directory: true,
            paths: PortablePaths {
                comfyui: "ComfyUI".to_string(),
                venv: "data/venv".to_string(),
                custom_nodes: "D:/SharedCustomNodes".to_string(),
                models: "ComfyUI/models".to_string(),
            },
        };
        let s = toml::to_string_pretty(&cfg).unwrap();
        let back: PortableConfig = toml::from_str(&s).unwrap();
        assert_eq!(cfg, back);
        assert!(s.contains("name = \"MyEnv\""));
        assert!(s.contains("port = 9999"));
        assert!(s.contains("override_base_directory = true"));
        assert!(s.contains("custom_nodes = \"D:/SharedCustomNodes\""));
    }

    #[test]
    fn test_is_under() {
        let base = PathBuf::from("/env");
        let inside = PathBuf::from("/env/ComfyUI/custom_nodes");
        let outside = PathBuf::from("/other/custom_nodes");

        assert!(is_under(&inside, &base));
        assert!(!is_under(&outside, &base));
    }

    #[test]
    fn test_default_paths_relative_to_env_root() {
        // 默认配置的所有路径都是相对 <env_root> 的
        let cfg = PortableConfig::default();
        // 关键断言：cache 子目录和 data 子目录都是相对路径
        assert!(!cfg.paths.venv.starts_with('/'));
        assert!(!cfg.paths.venv.starts_with('\\'));
    }
}
