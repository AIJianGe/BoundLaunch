//! venv 健康检查模块
//!
//! ## 设计目标
//!
//! **问题背景**（v3.x 实战案例）：装依赖时若 venv 中存在 pip 卸载残留
//! （`~xxx*` 损坏目录，例如 `~afetensors-0.8.0.dist-info`），pip 会：
//! 1. 输出 `WARNING: Ignoring invalid distribution ~xxx`
//! 2. **跳过**对应该包的安装/升级操作
//! 3. 留下不完整的包目录（缺少 torch.py / numpy.py 等子模块）
//!
//! 后果：ComfyUI 启动时 `import safetensors.torch` 失败 → 整个 custom_node
//! 加载链断裂 → 侧边栏"Manager"按钮消失、节点不出现。
//!
//! ## 检查维度
//!
//! 1. **损坏包检测**：扫描 `<venv>/Lib/site-packages/`，找 `~xxx*` 目录
//! 2. **关键 import 验证**：在 venv 里跑 `python -c "import xxx"`，验证 ComfyUI
//!    启动链路上的关键模块
//!
//! ## 自动修复
//!
//! - `clean_broken_distributions()`：删 `~xxx*` 目录
//! - `verify_critical_imports()`：跑 python 验证
//!
//! ## 调用时机
//!
//! - **被动**：用户点"修复 venv"按钮
//! - **主动**：`install_requirements` 之前先 clean（防止再次污染）
//! - **健康检查**：插件页打开时自动 health_check
//!
//! 详见 `PR/03-模块设计/04-PluginManager.md §5 venv 健康检查`

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use tokio::process::Command;

const CHECK_TIMEOUT_SECS: u64 = 30;

/// venv 健康检查结果
#[derive(Debug, Clone, Serialize)]
pub struct VenvHealthReport {
    /// venv 路径
    pub venv_path: PathBuf,
    /// site-packages 路径
    pub site_packages: PathBuf,
    /// 损坏包列表（`~xxx*`）
    pub broken_distributions: Vec<BrokenDistribution>,
    /// 关键 import 验证结果
    pub critical_imports: Vec<ImportCheckResult>,
    /// 总体状态
    pub status: VenvHealthStatus,
    /// 检查耗时（毫秒）
    pub elapsed_ms: u128,
}

/// 损坏包（`~xxx*` 目录）
#[derive(Debug, Clone, Serialize)]
pub struct BrokenDistribution {
    /// 目录名（如 `~afetensors-0.8.0.dist-info`）
    pub name: String,
    /// 完整路径
    pub path: PathBuf,
    /// 文件大小（字节）
    pub size_bytes: u64,
    /// 最后修改时间（ISO8601）
    pub last_modified: Option<String>,
}

/// 单个 import 验证结果
#[derive(Debug, Clone, Serialize)]
pub struct ImportCheckResult {
    /// 模块名（如 `safetensors.torch`）
    pub module: String,
    /// 是否可导入
    pub ok: bool,
    /// 错误信息（如果失败）
    pub error: Option<String>,
}

/// venv 总体状态
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VenvHealthStatus {
    /// 健康：没有损坏包 + 所有关键 import 通过
    Healthy,
    /// 有损坏包但 import 还正常（需要清理）
    HasBrokenDistributions,
    /// 关键 import 失败（严重）
    ImportFailed,
    /// 两者都有
    BrokenAndImportFailed,
    /// venv 目录不存在
    VenvNotFound,
    /// site-packages 目录不存在
    SitePackagesNotFound,
}

/// 关键 import 列表（ComfyUI 启动链路上必须可用的模块）
const CRITICAL_IMPORTS: &[&str] = &[
    "safetensors.torch", // ComfyUI nodes.py 第 21 行
    "safetensors",       // safetensors 基础包
    "folder_paths",      // ComfyUI 核心
    "comfy.samplers",    // ComfyUI 采样器
    "comfy.model_patcher", // ComfyUI 模型
    "server",            // ComfyUI HTTP server
    "nodes",             // ComfyUI 节点加载
];

/// 检测 site-packages 里的损坏包（`~xxx*` 目录）
///
/// pip 在异常情况下会先把目录重命名为 `~xxx` 临时目录，再尝试替换。
/// 如果替换失败，目录就留下来变成"损坏包"，pip 后续操作会忽略它。
///
/// **同时检查 `.dist-info` 与 `~xxx/` 两种损坏形式**。
pub fn detect_broken_distributions(site_packages: &Path) -> Vec<BrokenDistribution> {
    let mut result = Vec::new();
    let entries = match std::fs::read_dir(site_packages) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, ?site_packages, "read_dir failed");
            return result;
        }
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy().to_string();
        if !name_str.starts_with('~') {
            continue;
        }

        let path = entry.path();
        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, ?path, "metadata failed");
                continue;
            }
        };

        // 计算目录大小（仅顶层 + 1 层）
        let size_bytes = walk_size(&path, 2);

        // 最后修改时间
        let last_modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| {
                let secs = d.as_secs() as i64;
                chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_default()
            });

        result.push(BrokenDistribution {
            name: name_str,
            path,
            size_bytes,
            last_modified,
        });
    }

    // 按名字排序（方便用户查看）
    result.sort_by(|a, b| a.name.cmp(&b.name));
    result
}

/// 浅层计算目录大小（限制深度避免性能问题）
fn walk_size(path: &Path, max_depth: u8) -> u64 {
    fn walk(path: &Path, depth: u8) -> u64 {
        if depth == 0 {
            return 0;
        }
        let entries = match std::fs::read_dir(path) {
            Ok(e) => e,
            Err(_) => return 0,
        };
        let mut total = 0;
        for entry in entries.flatten() {
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if ft.is_file() {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            } else if ft.is_dir() {
                total += walk(&entry.path(), depth - 1);
            }
        }
        total
    }
    walk(path, max_depth)
}

/// 清理损坏包目录（`~xxx*`）
///
/// **安全**：
/// - 只删以 `~` 开头的目录（pip 临时目录约定）
/// - 跳过 `.gitkeep`、`.lock` 等隐藏文件
///
/// **返回值**：被删除的目录列表
pub fn clean_broken_distributions(site_packages: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    let entries = std::fs::read_dir(site_packages)?;

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with('~') {
            continue;
        }

        let path = entry.path();
        tracing::info!(?path, "removing broken distribution");
        if let Err(e) = std::fs::remove_dir_all(&path) {
            tracing::warn!(error = %e, ?path, "remove_dir_all failed");
        } else {
            removed.push(path);
        }
    }

    Ok(removed)
}

/// 验证关键 import（在 venv 里跑 python -c "import xxx"）
///
/// **性能**：每个 import 单独跑一次子进程（避免 import 失败导致后续跳过）
/// 7 个 import × ~0.5s ≈ 3-4s 总耗时。
///
/// **返回**：`Vec<ImportCheckResult>`，长度 == CRITICAL_IMPORTS.len()
pub async fn verify_critical_imports(venv_python: &Path) -> Vec<ImportCheckResult> {
    let mut results = Vec::with_capacity(CRITICAL_IMPORTS.len());

    for module in CRITICAL_IMPORTS {
        let module = *module;
        let result = run_import_check(venv_python, module).await;
        results.push(result);
    }

    results
}

/// 单个 import 验证（子进程 + 超时）
async fn run_import_check(venv_python: &Path, module: &str) -> ImportCheckResult {
    let mut cmd = Command::new(venv_python);
    cmd.args(["-c", &format!("import {}", module)]);

    let result = tokio::time::timeout(
        Duration::from_secs(CHECK_TIMEOUT_SECS),
        cmd.output(),
    )
    .await;

    match result {
        Ok(Ok(output)) if output.status.success() => ImportCheckResult {
            module: module.to_string(),
            ok: true,
            error: None,
        },
        Ok(Ok(output)) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            ImportCheckResult {
                module: module.to_string(),
                ok: false,
                error: Some(truncate_error(&stderr)),
            }
        }
        Ok(Err(e)) => ImportCheckResult {
            module: module.to_string(),
            ok: false,
            error: Some(format!("spawn failed: {}", e)),
        },
        Err(_) => ImportCheckResult {
            module: module.to_string(),
            ok: false,
            error: Some(format!("timeout ({}s)", CHECK_TIMEOUT_SECS)),
        },
    }
}

/// 截断错误信息（避免返回几 MB 的 pip 输出）
fn truncate_error(s: &str) -> String {
    const MAX_LEN: usize = 500;
    if s.len() <= MAX_LEN {
        s.to_string()
    } else {
        format!("{}... (truncated, total {} bytes)", &s[..MAX_LEN], s.len())
    }
}

/// 计算 venv 的 site-packages 路径
///
/// **跨平台**：
/// - Windows: `<venv>/Lib/site-packages`
/// - Unix: `<venv>/lib/python3.X/site-packages`
pub fn site_packages_path(venv_path: &Path) -> PathBuf {
    if cfg!(windows) {
        venv_path.join("Lib").join("site-packages")
    } else {
        // Unix: lib/pythonX.Y/site-packages
        // 简化处理：尝试找 python3.X 子目录
        let lib = venv_path.join("lib");
        if let Ok(entries) = std::fs::read_dir(&lib) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with("python") {
                    return entry.path().join("site-packages");
                }
            }
        }
        // fallback
        lib.join("site-packages")
    }
}

/// 完整健康检查（检测 + 验证 + 汇总）
///
/// **典型调用**：
/// ```rust,ignore
/// let report = check_venv_health(&venv_path).await;
/// if report.status != VenvHealthStatus::Healthy { ... }
/// ```
pub async fn check_venv_health(venv_path: &Path) -> VenvHealthReport {
    let start = std::time::Instant::now();
    let site_packages = site_packages_path(venv_path);

    // 1. venv / site-packages 存在性
    if !venv_path.exists() {
        return VenvHealthReport {
            venv_path: venv_path.to_path_buf(),
            site_packages: site_packages.clone(),
            broken_distributions: vec![],
            critical_imports: vec![],
            status: VenvHealthStatus::VenvNotFound,
            elapsed_ms: start.elapsed().as_millis(),
        };
    }
    if !site_packages.exists() {
        return VenvHealthReport {
            venv_path: venv_path.to_path_buf(),
            site_packages: site_packages.clone(),
            broken_distributions: vec![],
            critical_imports: vec![],
            status: VenvHealthStatus::SitePackagesNotFound,
            elapsed_ms: start.elapsed().as_millis(),
        };
    }

    // 2. 检测损坏包
    let broken = detect_broken_distributions(&site_packages);

    // 3. 验证关键 import
    let venv_python = if cfg!(windows) {
        venv_path.join("Scripts").join("python.exe")
    } else {
        venv_path.join("bin").join("python")
    };

    let imports = if venv_python.exists() {
        verify_critical_imports(&venv_python).await
    } else {
        // python 不存在 → 全部失败
        CRITICAL_IMPORTS
            .iter()
            .map(|m| ImportCheckResult {
                module: m.to_string(),
                ok: false,
                error: Some("python binary not found".to_string()),
            })
            .collect()
    };

    // 4. 汇总状态
    let has_broken = !broken.is_empty();
    let has_import_failure = imports.iter().any(|i| !i.ok);
    let status = match (has_broken, has_import_failure) {
        (true, true) => VenvHealthStatus::BrokenAndImportFailed,
        (true, false) => VenvHealthStatus::HasBrokenDistributions,
        (false, true) => VenvHealthStatus::ImportFailed,
        (false, false) => VenvHealthStatus::Healthy,
    };

    VenvHealthReport {
        venv_path: venv_path.to_path_buf(),
        site_packages,
        broken_distributions: broken,
        critical_imports: imports,
        status,
        elapsed_ms: start.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// 临时目录 helper
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("venv_health_test_{}", name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_detect_broken_distributions_empty() {
        let site_packages = temp_dir("empty");
        let result = detect_broken_distributions(&site_packages);
        assert!(result.is_empty(), "应该没有损坏包");
        fs::remove_dir_all(site_packages).ok();
    }

    #[test]
    fn test_detect_broken_distributions_finds_tilde_dirs() {
        let site_packages = temp_dir("with_tilde");
        // 创建一个 ~xxx 损坏包
        fs::create_dir(site_packages.join("~afetensors-0.8.0.dist-info")).unwrap();
        fs::create_dir(site_packages.join("~broken-pkg")).unwrap();
        // 正常包不应该被识别
        fs::create_dir(site_packages.join("safetensors")).unwrap();
        fs::create_dir(site_packages.join("normal-pkg.dist-info")).unwrap();

        let result = detect_broken_distributions(&site_packages);
        assert_eq!(result.len(), 2);
        let names: Vec<_> = result.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"~afetensors-0.8.0.dist-info"));
        assert!(names.contains(&"~broken-pkg"));
        fs::remove_dir_all(site_packages).ok();
    }

    #[test]
    fn test_clean_broken_distributions() {
        let site_packages = temp_dir("clean");
        fs::create_dir(site_packages.join("~bad1")).unwrap();
        fs::create_dir(site_packages.join("~bad2")).unwrap();
        fs::create_dir(site_packages.join("good")).unwrap();

        let removed = clean_broken_distributions(&site_packages).unwrap();
        assert_eq!(removed.len(), 2);
        assert!(!site_packages.join("~bad1").exists());
        assert!(!site_packages.join("~bad2").exists());
        assert!(site_packages.join("good").exists(), "正常包不应该被删");
        fs::remove_dir_all(site_packages).ok();
    }

    #[test]
    fn test_site_packages_path_windows() {
        if cfg!(windows) {
            let venv = PathBuf::from("D:/test/venv");
            let sp = site_packages_path(&venv);
            assert_eq!(sp, PathBuf::from("D:/test/venv/Lib/site-packages"));
        }
    }

    #[test]
    fn test_truncate_error() {
        let short = "short error";
        assert_eq!(truncate_error(short), "short error");

        let long = "a".repeat(1000);
        let result = truncate_error(&long);
        assert!(result.len() < long.len());
        assert!(result.contains("truncated"));
    }
}
