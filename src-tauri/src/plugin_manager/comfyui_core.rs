//! ComfyUI 核心依赖管理模块
//!
//! ## 设计目标
//!
//! 解决 "ComfyUI 核心切了版本 / 更新了 commit 后没人装依赖" 的问题：
//!
//! 1. **ComfyUI-Manager 面板切 ComfyUI 核心版本**：只 `git checkout`，不装依赖
//! 2. **ComfyUI 切到 master 后**：comfy_kitchen / comfy_aimdo / 各种子包可能变了
//! 3. **重启 ComfyUI 后**：只检测，**不自动装**
//!
//! **本模块提供 3 个能力**：
//! - `compute_requirements_hash()`：用 SHA256 计算 requirements.txt 的内容指纹
//! - `check_comfyui_requirements()`：检测是否需要重装（hash 变了 / 首次 / 强制）
//! - `install_comfyui_requirements()`：执行 `pip install -r ComfyUI/requirements.txt`
//!
//! ## 状态文件
//!
//! - `<custom_nodes_parent>/.comfyui_requirements_hash`（与 `.trash` 同级）
//! - 内容：上一次成功装依赖时的 hash（40 字节 hex = 80 字符）
//! - **不存在 = 从未装过**（视为 needs_install）
//!
//! ## 调用时机
//!
//! 1. **用户点"启动 ComfyUI"**：先跑 pre_check，再启动
//! 2. **手动点"装核心依赖"按钮**：直接装
//! 3. **launcher 启动时**：可选地静默跑 pre_check 提示用户
//!
//! ## 复用 plugin 装依赖的进度 / 日志
//!
//! 同一个 `plugin_progress` 事件 + `plugin_progress_log` 事件：
//! - `plugin` 字段固定为 `"__comfyui_core__"`（特殊名字）
//! - 前端识别这个特殊名字时显示 "ComfyUI 核心" 而不是插件名
//!
//! 详见 `PR/03-模块设计/04-PluginManager.md §6 ComfyUI 核心依赖管理`

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::venv_health;
use crate::common::process_util;

/// 特殊 plugin 名前缀（用于装 ComfyUI 核心依赖时复用 plugin 进度事件）
pub const COMFYUI_CORE_PLUGIN_KEY: &str = "__comfyui_core__";

/// 状态文件名（保存 requirements.txt hash）
const HASH_FILE_NAME: &str = ".comfyui_requirements_hash";

/// ComfyUI 核心依赖状态
#[derive(Debug, Clone, Serialize)]
pub struct ComfyUICoreRequirementsStatus {
    /// ComfyUI 核心目录路径
    pub comfyui_root: PathBuf,
    /// requirements.txt 路径（None 表示不存在）
    pub requirements_path: Option<PathBuf>,
    /// 当前 hash
    pub current_hash: Option<String>,
    /// 上次装成功的 hash
    pub last_installed_hash: Option<String>,
    /// 是否需要重装
    pub needs_install: bool,
    /// 原因（人类可读）
    pub reason: String,
    /// 距离上次装多久（秒）
    pub last_install_seconds_ago: Option<u64>,
    /// 耗时（毫秒）
    pub elapsed_ms: u128,
}

/// 启动 ComfyUI 前的完整检查
#[derive(Debug, Clone, Serialize)]
pub struct PreLaunchCheck {
    /// ComfyUI 核心依赖状态
    pub core_requirements: ComfyUICoreRequirementsStatus,
    /// 待装依赖的 custom node 列表（requirements.txt 存在但未装）
    pub plugins_needing_install: Vec<PluginInstallNeeded>,
    /// 是否所有都 OK
    pub all_ok: bool,
    /// 总体耗时（毫秒）
    pub elapsed_ms: u128,
}

/// 待装依赖的 plugin 信息
#[derive(Debug, Clone, Serialize)]
pub struct PluginInstallNeeded {
    /// plugin 名
    pub name: String,
    /// plugin 路径
    pub path: PathBuf,
    /// 当前 commit
    pub commit: Option<String>,
    /// 当前 ref (tag/branch/commit short)
    pub current_ref: Option<String>,
}

/// 计算 requirements.txt 的 SHA256 hash
///
/// **不规范化**：raw 内容直接 hash（避免与 pip 行为不一致）
/// **空文件**：返回 sha256("") 的固定值（`e3b0c44...`），方便对比
pub fn compute_requirements_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let result = hasher.finalize();
    // 64 字节 hex
    format!("{:x}", result)
}

/// 读取 requirements.txt 内容并计算 hash
pub fn hash_requirements_file(path: &Path) -> Result<String, std::io::Error> {
    let content = std::fs::read_to_string(path)?;
    Ok(compute_requirements_hash(&content))
}

/// 状态文件路径：`<custom_nodes_parent>/.comfyui_requirements_hash`
///
/// **设计**：
/// - 放在 custom_nodes 父目录（与 `.trash` 同级）
/// - **不**放在 ComfyUI 核心目录（避免污染 ComfyUI git 状态）
/// - **不**放在 venv（venv 是 launcher 管理的）
pub fn hash_file_path(custom_nodes_path: &Path) -> PathBuf {
    custom_nodes_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| custom_nodes_path.to_path_buf())
        .join(HASH_FILE_NAME)
}

/// 读取上次装成功的 hash
///
/// **注意**：返回 None 表示从未装过（不是错误）
pub fn read_last_installed_hash(custom_nodes_path: &Path) -> Option<String> {
    let path = hash_file_path(custom_nodes_path);
    let content = std::fs::read_to_string(&path).ok()?;
    // 取第一行（避免行尾换行/空格干扰）
    let hash = content.lines().next()?.trim();
    if hash.is_empty() {
        return None;
    }
    Some(hash.to_string())
}

/// 写入装成功的 hash
pub fn write_last_installed_hash(
    custom_nodes_path: &Path,
    hash: &str,
) -> Result<(), std::io::Error> {
    let path = hash_file_path(custom_nodes_path);
    // 保证父目录存在（custom_nodes 父目录一定存在，防御性写）
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, hash.as_bytes())
}

/// 删除状态文件（reset 用）
pub fn clear_hash(custom_nodes_path: &Path) -> Result<(), std::io::Error> {
    let path = hash_file_path(custom_nodes_path);
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

/// 检查 ComfyUI 核心依赖状态
///
/// **检测逻辑**：
/// 1. requirements.txt 不存在 → needs_install = false（不装）
/// 2. 从未装过（hash 文件不存在）→ needs_install = true（首次）
/// 3. hash 变了 → needs_install = true（内容变了）
/// 4. hash 一样 → needs_install = false
/// 5. force_reinstall = true → 无脑 needs_install = true
pub fn check_comfyui_requirements(
    comfyui_root: &Path,
    custom_nodes_path: &Path,
    force_reinstall: bool,
) -> ComfyUICoreRequirementsStatus {
    let start = Instant::now();
    let requirements_path = comfyui_root.join("requirements.txt");
    let current_hash = if requirements_path.exists() {
        hash_requirements_file(&requirements_path).ok()
    } else {
        None
    };
    let last_installed_hash = read_last_installed_hash(custom_nodes_path);

    let (needs_install, reason) = compute_needs_install(
        &requirements_path,
        current_hash.as_deref(),
        last_installed_hash.as_deref(),
        force_reinstall,
    );

    ComfyUICoreRequirementsStatus {
        comfyui_root: comfyui_root.to_path_buf(),
        requirements_path: requirements_path.exists().then(|| requirements_path),
        current_hash,
        last_installed_hash,
        needs_install,
        reason,
        last_install_seconds_ago: None, // 不存时间戳，只 hash 对比即可
        elapsed_ms: start.elapsed().as_millis(),
    }
}

/// 内部：判断是否需要重装
///
/// **抽出为独立函数**便于单元测试
fn compute_needs_install(
    requirements_path: &Path,
    current_hash: Option<&str>,
    last_installed_hash: Option<&str>,
    force_reinstall: bool,
) -> (bool, String) {
    if force_reinstall {
        return (true, "强制重装（force_reinstall=true）".to_string());
    }
    if !requirements_path.exists() {
        return (false, "requirements.txt 不存在，无需装".to_string());
    }
    match (current_hash, last_installed_hash) {
        (None, _) => (false, "requirements.txt 读取失败".to_string()),
        (Some(_), None) => (true, "首次安装 ComfyUI 核心依赖".to_string()),
        (Some(cur), Some(last)) if cur != last => {
            (true, "ComfyUI 核心 requirements.txt 内容已变化".to_string())
        }
        (Some(cur), Some(last)) if cur == last => {
            (false, "依赖已是最新（hash 一致）".to_string())
        }
        _ => (false, "未知状态".to_string()),
    }
}

/// 启动 ComfyUI 前的完整检查
///
/// **检查 3 件事**：
/// 1. ComfyUI 核心 requirements 是否需要装
/// 2. 所有 custom node 中 requirements.txt 存在但 `requirements_installed = false` 的
/// 3. 综合判断 `all_ok`
pub fn pre_launch_check(
    comfyui_root: &Path,
    custom_nodes_path: &Path,
    force_reinstall: bool,
    scan_plugins_fn: impl Fn(&Path) -> Vec<PluginInstallNeeded>,
) -> PreLaunchCheck {
    let start = Instant::now();
    let core_requirements =
        check_comfyui_requirements(comfyui_root, custom_nodes_path, force_reinstall);
    let plugins_needing_install = scan_plugins_fn(custom_nodes_path);
    let all_ok = !core_requirements.needs_install && plugins_needing_install.is_empty();
    PreLaunchCheck {
        core_requirements,
        plugins_needing_install,
        all_ok,
        elapsed_ms: start.elapsed().as_millis(),
    }
}

/// 装 ComfyUI 核心依赖
///
/// **复用 plugin_install_requirements 的核心逻辑**：
/// - venv_health::clean_broken_distributions 前置清理
/// - pip install -r 实时进度解析
/// - 装完 venv_health::verify_critical_imports 验证
/// - 失败时 emit 错误事件，成功时写 hash
///
/// **返回值**：成功时 hash 用于写到状态文件
pub async fn install_comfyui_requirements(
    comfyui_root: &Path,
    custom_nodes_path: &Path,
    venv_path: &Path,
    force_reinstall: bool,
    on_log: impl Fn(&str) + Send + Sync + 'static,
) -> Result<String, ComfyUIInstallError> {
    let requirements = comfyui_root.join("requirements.txt");
    if !requirements.exists() {
        return Err(ComfyUIInstallError::RequirementsNotFound);
    }

    // 1. 算 hash（成功后再写）
    let hash = hash_requirements_file(&requirements)
        .map_err(|e| ComfyUIInstallError::Io(format!("read requirements: {}", e)))?;

    // 2. 找 python
    let venv_python = if cfg!(windows) {
        venv_path.join("Scripts").join("python.exe")
    } else {
        venv_path.join("bin").join("python")
    };
    if !venv_python.exists() {
        return Err(ComfyUIInstallError::VenvPythonNotFound(venv_python));
    }

    // 3. 前置：清理 site-packages 损坏包（防御）
    let site_packages = venv_health::site_packages_path(venv_path);
    if site_packages.exists() {
        if let Ok(removed) = venv_health::clean_broken_distributions(&site_packages) {
            if !removed.is_empty() {
                on_log(&format!(
                    "[pre-install] 已清理 {} 个损坏包: {:?}",
                    removed.len(),
                    removed
                        .iter()
                        .filter_map(|p| p.file_name())
                        .filter_map(|n| n.to_str())
                        .collect::<Vec<_>>()
                ));
            }
        }
    }

    // 4. pip install
    let mut cmd = process_util::new_command(&venv_python);
    cmd.args(["-u", "-m", "pip", "install", "-r"]);
    cmd.arg(&requirements);
    if force_reinstall {
        cmd.arg("--force-reinstall");
    }
    // 不传 --no-deps，让 pip 完整处理依赖

    on_log(&format!(
        "[install] 执行: python -m pip install -r {} {}{}",
        requirements.display(),
        if force_reinstall { "--force-reinstall " } else { "" },
        format!("(hash={})", &hash[..16])
    ));

    let output = cmd
        .output()
        .await
        .map_err(|e| ComfyUIInstallError::SpawnFailed(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        on_log(&format!("[error] pip 退出 {:?}\nstderr: {}\nstdout: {}",
            output.status.code(), stderr, stdout));
        return Err(ComfyUIInstallError::PipFailed(format!(
            "exit code {:?}",
            output.status.code()
        )));
    }

    on_log(&format!("[ok] pip install 退出 0, hash={}", hash));

    // 5. 写 hash
    write_last_installed_hash(custom_nodes_path, &hash)
        .map_err(|e| ComfyUIInstallError::Io(format!("write hash: {}", e)))?;

    // 6. 关键 import 验证（非阻塞，失败只 warn）
    let venv_python_check = venv_python.clone();
    let import_results = venv_health::verify_critical_imports(&venv_python_check).await;
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
        on_log(&format!(
            "[warn] pip 退出 0 但关键 import 失败: {}",
            summary
        ));
        // 不返回错误（不阻塞），让前端自己判断
    } else {
        on_log("[ok] 关键 import 验证全部通过");
    }

    Ok(hash)
}

/// 装核心依赖的错误类型
#[derive(Debug, thiserror::Error)]
pub enum ComfyUIInstallError {
    #[error("requirements.txt not found")]
    RequirementsNotFound,

    #[error("venv python not found: {0}")]
    VenvPythonNotFound(PathBuf),

    #[error("io error: {0}")]
    Io(String),

    #[error("pip spawn failed: {0}")]
    SpawnFailed(String),

    #[error("pip install failed: {0}")]
    PipFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("comfyui_core_test_{}", name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_compute_requirements_hash_basic() {
        let h1 = compute_requirements_hash("torch>=2.0\n");
        let h2 = compute_requirements_hash("torch>=2.0\n");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // SHA256 hex

        // 内容变了 hash 也变
        let h3 = compute_requirements_hash("torch>=2.1\n");
        assert_ne!(h1, h3);
    }

    #[test]
    fn test_compute_requirements_hash_empty() {
        // 空字符串的 sha256 应该是固定值
        let h = compute_requirements_hash("");
        assert_eq!(h.len(), 64);
        // e3b0c44... 是 sha256("") 的标准值
        assert!(h.starts_with("e3b0c44"));
    }

    #[test]
    fn test_hash_file_path() {
        let custom_nodes = PathBuf::from("D:/test/ComfyUI/custom_nodes");
        let p = hash_file_path(&custom_nodes);
        assert_eq!(p, PathBuf::from("D:/test/ComfyUI/.comfyui_requirements_hash"));
    }

    #[test]
    fn test_hash_file_roundtrip() {
        let dir = temp_dir("hash_rt");
        let custom_nodes = dir.join("custom_nodes");
        fs::create_dir_all(&custom_nodes).unwrap();

        // 首次：没有文件
        assert_eq!(read_last_installed_hash(&custom_nodes), None);

        // 写入
        let hash = "abc1234567890abcdef";
        write_last_installed_hash(&custom_nodes, hash).unwrap();

        // 读取
        let read = read_last_installed_hash(&custom_nodes);
        assert_eq!(read, Some(hash.to_string()));

        // 清理
        clear_hash(&custom_nodes).unwrap();
        assert_eq!(read_last_installed_hash(&custom_nodes), None);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_compute_needs_install_never_installed() {
        let req = PathBuf::from("/tmp/req.txt");
        let (need, reason) = compute_needs_install(&req, Some("hash1"), None, false);
        assert!(need);
        assert!(reason.contains("首次"));
    }

    #[test]
    fn test_compute_needs_install_hash_unchanged() {
        let req = PathBuf::from("/tmp/req.txt");
        let (need, reason) = compute_needs_install(&req, Some("hash1"), Some("hash1"), false);
        assert!(!need);
        assert!(reason.contains("已是最新"));
    }

    #[test]
    fn test_compute_needs_install_hash_changed() {
        let req = PathBuf::from("/tmp/req.txt");
        let (need, reason) = compute_needs_install(&req, Some("hash_new"), Some("hash_old"), false);
        assert!(need);
        assert!(reason.contains("已变化"));
    }

    #[test]
    fn test_compute_needs_install_force() {
        let req = PathBuf::from("/tmp/req.txt");
        let (need, reason) = compute_needs_install(&req, Some("h"), Some("h"), true);
        assert!(need);
        assert!(reason.contains("强制"));
    }

    #[test]
    fn test_compute_needs_install_no_requirements() {
        let req = PathBuf::from("/tmp/nonexistent_req.txt");
        let (need, reason) = compute_needs_install(&req, None, None, false);
        assert!(!need);
        assert!(reason.contains("不存在"));
    }

    #[test]
    fn test_compute_needs_install_read_failure() {
        let req = PathBuf::from("/tmp/req.txt");
        // current_hash = None 表示读取失败
        let (need, reason) = compute_needs_install(&req, None, Some("h"), false);
        assert!(!need);
        assert!(reason.contains("读取失败"));
    }
}
