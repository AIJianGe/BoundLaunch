//! 绿色版（portable）模式路径解析
//!
//! ## 设计目标
//!
//! 让 BoundLaunch.exe **完全独立运行**，不依赖任何外部配置：
//!
//! - 所有路径相对 `<exe_dir>` 解析（即 portable.dat 所在目录）
//! - 启动时自动写默认 `launcher-portable.dat`
//! - 支持 custom_nodes 在 ComfyUI 内（默认）或外（高级）
//!
//! ## 核心原则
//!
//! - BoundLaunch.exe 不知道自己是不是"绿色版"
//! - 它只读自己目录的 `launcher-portable.dat`
//! - 找不到 → 自动创建默认配置（首次启动）
//! - 找到 → 解析为绝对路径
//!
//! ## 路径优先级
//!
//! 1. **硬规则**：所有路径相对 `<exe_dir>` 解析（除了显式绝对路径）
//! 2. **可覆盖**：launcher-portable.dat 里的 [paths] 节可以覆盖
//! 3. **不可覆盖**：launcher 自己（BoundLaunch.exe）必须在 <exe_dir>
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

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// portable.dat 文件名（必须和 BoundLaunch.exe 在同一目录）
pub const PORTABLE_CONFIG_FILENAME: &str = "launcher-portable.dat";

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
/// boundlaunch_data = ".boundlaunch"
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
fn default_port() -> u16 { 8188 }

/// 路径配置（相对 <exe_dir>）
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

    /// launcher 私有数据（SQLite、日志、配置）
    #[serde(default = "default_boundlaunch_data")]
    pub boundlaunch_data: String,
}

fn default_comfyui() -> String { "ComfyUI".to_string() }
fn default_venv() -> String { "data/venv".to_string() }
fn default_custom_nodes() -> String { "ComfyUI/custom_nodes".to_string() }
fn default_models() -> String { "ComfyUI/models".to_string() }
fn default_boundlaunch_data() -> String { ".boundlaunch".to_string() }

impl Default for PortablePaths {
    fn default() -> Self {
        Self {
            comfyui: default_comfyui(),
            venv: default_venv(),
            custom_nodes: default_custom_nodes(),
            models: default_models(),
            boundlaunch_data: default_boundlaunch_data(),
        }
    }
}

impl Default for PortableConfig {
    fn default() -> Self {
        Self {
            version: default_version(),
            name: String::new(),  // 空 = 用目录名兜底
            port: default_port(),
            override_base_directory: false,
            paths: PortablePaths::default(),
        }
    }
}

/// 解析后的环境路径（绝对路径）
///
/// 这是 launcher 启动时实际使用的路径集合，所有路径都是绝对路径。
#[derive(Debug, Clone)]
pub struct ResolvedEnvPaths {
    /// 环境根目录（= exe 所在目录 = portable.dat 所在目录）
    pub env_root: PathBuf,
    /// 环境名
    pub env_name: String,
    /// ComfyUI 启动端口
    pub port: u16,
    /// ComfyUI 核心目录
    pub comfyui_root: PathBuf,
    /// venv
    pub venv_path: PathBuf,
    /// custom_nodes 绝对路径
    pub custom_nodes: PathBuf,
    /// custom_nodes 是否在 ComfyUI 内
    pub custom_nodes_in_comfyui: bool,
    /// 模型目录
    pub models_dir: PathBuf,
    /// launcher 私有数据目录
    pub boundlaunch_data: PathBuf,
    /// launcher 配置
    pub config_path: PathBuf,
    /// SQLite 数据库
    pub database_path: PathBuf,
    /// logs 目录
    pub logs_dir: PathBuf,
    /// **v3.x 新增**：ComfyUI session 目录（每个实例独立，多实例隔离）
    ///
    /// 用于 `__COMFY_CLI_SESSION__` 协议：
    /// - 每个 ComfyUI 进程生成一个 `<sessions_dir>/<random>.session` 文件
    /// - ComfyUI-Manager 检测到 `__COMFY_CLI_SESSION__` 环境变量后
    ///   → 写 `<session_path>.reboot` 标志 + `exit(0)`
    /// - 客户端检测 `.reboot` → 自动 respawn（无缝重启）
    ///
    /// **多实例隔离保证**：
    /// - 路径 = `<exe_dir>/.boundlaunch/sessions/`
    /// - 复制目录到新位置 → 新实例用自己的 sessions/ → 互不影响 ✅
    /// - 同一实例多 ComfyUI 进程：文件名带随机后缀 → 不冲突 ✅
    pub sessions_dir: PathBuf,
    /// ComfyUI 启动时是否要传 --base-directory
    pub override_base_directory: bool,
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

/// 找到 BoundLaunch.exe 所在目录
///
/// **重要**：必须能拿到 exe 路径，**否则启动失败**
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
pub fn load_or_create() -> Result<(PortableConfig, PathBuf), EnvPathsError> {
    let path = portable_config_path()?;
    if path.exists() {
        let content = std::fs::read_to_string(&path)
            .map_err(|e| EnvPathsError::ParseError(e.to_string()))?;
        let cfg: PortableConfig = toml::from_str(&content)
            .map_err(|e| EnvPathsError::ParseError(e.to_string()))?;
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
/// **规则**：
/// - 绝对路径 → 原样返回
/// - 相对路径 → 相对 <base>
fn resolve_relative(base: &Path, rel: &str) -> PathBuf {
    let p = Path::new(rel);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(rel)
    }
}

/// 解析为完整路径集
///
/// **这是 launcher 启动时唯一应该调用的函数**
pub fn resolve() -> Result<ResolvedEnvPaths, EnvPathsError> {
    let (cfg, portable_path) = load_or_create()?;
    let env_root = find_exe_dir()?;
    if !env_root.exists() {
        return Err(EnvPathsError::EnvRootNotFound(env_root));
    }

    let comfyui_root = resolve_relative(&env_root, &cfg.paths.comfyui);
    let custom_nodes = resolve_relative(&env_root, &cfg.paths.custom_nodes);
    let boundlaunch_data = resolve_relative(&env_root, &cfg.paths.boundlaunch_data);

    // 关键判断：custom_nodes 是否在 ComfyUI 内
    // 用 canonicalize 处理 "ComfyUI/custom_nodes" vs "<env>/ComfyUI/custom_nodes" 的等价性
    let custom_nodes_in_comfyui = is_under(&custom_nodes, &comfyui_root);

    // 自动推断 override_base_directory：
    //   - 用户显式设置 → 用用户的
    //   - 用户没设置（false）+ custom_nodes 在 ComfyUI 外 → 自动设为 true
    let override_base_directory = if cfg.override_base_directory {
        true
    } else {
        !custom_nodes_in_comfyui
    };

    let env_name = if cfg.name.is_empty() {
        env_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("default")
            .to_string()
    } else {
        cfg.name
    };

    Ok(ResolvedEnvPaths {
        env_root: env_root.clone(),
        env_name,
        port: cfg.port,
        comfyui_root,
        venv_path: resolve_relative(&env_root, &cfg.paths.venv),
        custom_nodes,
        custom_nodes_in_comfyui,
        models_dir: resolve_relative(&env_root, &cfg.paths.models),
        boundlaunch_data: boundlaunch_data.clone(),
        config_path: boundlaunch_data.join("config.toml"),
        database_path: boundlaunch_data.join("launcher.db"),
        logs_dir: boundlaunch_data.join("logs"),
        // **v3.x 新增**：ComfyUI session 目录
        // 放在 `<exe_dir>/.boundlaunch/sessions/`，与 WebView2 UserData 平行
        // 复制目录时整个 .boundlaunch/ 一起被复制 → 多实例完全隔离
        sessions_dir: env_root.join(".boundlaunch").join("sessions"),
        override_base_directory,
        portable_config_path: portable_path,
    })
}

/// 判断 child 是否在 parent 下（路径前缀关系）
///
/// **实现**：
/// - 都 canonicalize（如果存在）再比较
/// - 都不存在时直接字符串比较（处理"还没创建"的情况）
fn is_under(child: &Path, parent: &Path) -> bool {
    let child_canon = child.canonicalize().unwrap_or_else(|_| child.to_path_buf());
    let parent_canon = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
    child_canon.starts_with(&parent_canon)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 创建一个临时目录，测试用
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
        assert_eq!(cfg.port, 8188);
        assert_eq!(cfg.paths.comfyui, "ComfyUI");
        assert_eq!(cfg.paths.venv, "data/venv");
        assert_eq!(cfg.paths.custom_nodes, "ComfyUI/custom_nodes");
        assert_eq!(cfg.paths.models, "ComfyUI/models");
        assert_eq!(cfg.paths.boundlaunch_data, ".boundlaunch");
        assert!(!cfg.override_base_directory);
    }

    #[test]
    fn test_resolve_relative_path() {
        let base = PathBuf::from("/test/base");
        // 相对路径 → 拼接
        assert_eq!(
            resolve_relative(&base, "ComfyUI"),
            PathBuf::from("/test/base/ComfyUI")
        );
        // 绝对路径 → 原样
        assert_eq!(
            resolve_relative(&base, "/abs/path"),
            PathBuf::from("/abs/path")
        );
        // 相对路径含子目录
        assert_eq!(
            resolve_relative(&base, "data/venv"),
            PathBuf::from("/test/base/data/venv")
        );
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
                venv: "venv".to_string(),
                custom_nodes: "D:/SharedCustomNodes".to_string(),
                models: "ComfyUI/models".to_string(),
                boundlaunch_data: ".data".to_string(),
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
    fn test_temp_dir_creation() {
        let dir = temp_dir("basic");
        assert!(dir.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
