//! 插件列表扫描与启停
//!
//! 设计：纯同步函数，由 `mod.rs` 用 `spawn_blocking` 包裹。
//!
//! 性能要点（详见 `PR/03-模块设计/04-PluginManager.md §6`）：
//! - 扫描时不读文件内容，仅看目录结构 + `.git` 元信息
//! - 描述信息延迟加载（用户展开时再读 `__init__.py`）

use std::path::Path;

use git2::Repository;

use super::git_ops;
use super::models::{PluginError, PluginInfo};

/// `.disabled` 后缀（ComfyUI 约定的禁用插件标记）
pub const DISABLED_SUFFIX: &str = ".disabled";

/// 扫描 custom_nodes 目录，列出所有插件
///
/// - 跳过隐藏目录（`.trash` / `.git` 等）
/// - 识别 `.disabled` 后缀判断启停状态
/// - 读 git 元信息（commit / branch / ref / detached / remote_url / dirty）
/// - 描述信息从 `pyproject.toml` / `__init__.py` 读取（简单实现）
pub fn scan_plugins(custom_nodes_path: &Path) -> Result<Vec<PluginInfo>, PluginError> {
    let mut plugins = vec![];

    if !custom_nodes_path.exists() {
        return Ok(vec![]);
    }

    let entries = match std::fs::read_dir(custom_nodes_path) {
        Ok(e) => e,
        Err(_) => return Ok(vec![]),
    };

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let dir_name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };

        // 跳过隐藏目录（.trash / .DS_Store 等）
        if dir_name.starts_with('.') {
            continue;
        }

        // 识别 .disabled 后缀
        let (plugin_name, enabled) = if let Some(stripped) = dir_name.strip_suffix(DISABLED_SUFFIX)
        {
            (stripped.to_string(), false)
        } else {
            (dir_name.clone(), true)
        };

        // 读 git 元信息
        let (current_commit, current_branch, current_ref, is_detached, git_url, has_local_changes) =
            match Repository::open(&path) {
                Ok(repo) => {
                    let commit = git_ops::current_commit(&repo).unwrap_or_default();
                    let branch = git_ops::current_branch(&repo).unwrap_or(None);
                    let is_detached = branch.is_none() && !commit.is_empty();
                    // v3.x：解析当前 ref（tag 优先，再 branch，最后 commit short）
                    let current_ref = resolve_current_ref(&repo);
                    let url = git_ops::remote_url(&repo);
                    let dirty = git_ops::has_local_changes(&repo).unwrap_or(false);
                    (commit, branch, current_ref, is_detached, url, dirty)
                }
                Err(_) => (String::new(), None, None, false, None, false),
            };

        // 描述信息（延迟加载策略可后续优化，本期直接读）
        let description = read_description(&path);

        // requirements_installed：简单判断
        // - 无 requirements.txt → 视为已满足（无需安装）
        // - 有 requirements.txt → 暂视为未安装（真实判断需 venv pip list 比对，性能成本高）
        let requirements_installed = !path.join("requirements.txt").exists();

        // v3.x：读 backup_commit（持久化在 `<plugin>/.launcher_backup_commit`）
        let backup_commit = std::fs::read_to_string(path.join(".launcher_backup_commit"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()));

        plugins.push(PluginInfo {
            name: plugin_name,
            dir_name,
            enabled,
            git_url,
            current_commit,
            current_branch,
            current_ref,
            backup_commit,
            is_detached,
            has_updates: None,
            has_local_changes,
            installed_at: read_installed_at(&path),
            description,
            requirements_installed,
        });
    }

    // 按名字排序，保证多次扫描结果稳定
    plugins.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(plugins)
}

/// 解析当前 HEAD 对应的可读 ref 名
///
/// 优先级：tag > branch > commit short
fn resolve_current_ref(repo: &Repository) -> Option<String> {
    let head = repo.head().ok()?;
    let commit = head.peel_to_commit().ok()?;
    let commit_id = commit.id().to_string();

    // 1. 检查是否在某个 tag 上
    let tags = repo.tag_names(None).ok()?;
    for tag in tags.iter().flatten() {
        if tag.ends_with("^{}") {
            continue;
        }
        let ref_name = format!("refs/tags/{}", tag);
        if let Ok(tag_ref) = repo.find_reference(&ref_name) {
            if let Ok(tag_commit) = tag_ref.peel_to_commit() {
                if tag_commit.id().to_string() == commit_id {
                    return Some(tag.to_string());
                }
            }
        }
    }

    // 2. 检查是否在某个 branch 上
    if head.is_branch() {
        if let Some(s) = head.shorthand() {
            return Some(s.to_string());
        }
    }

    // 3. fallback: commit short
    Some(commit_id[..7].to_string())
}

/// 启停插件（rename `<name>` ↔ `<name>.disabled`）
///
/// 幂等：当前状态等于目标状态时不操作。
/// 不存在：返回 `NotFound`。
pub fn toggle_plugin(
    custom_nodes_path: &Path,
    plugin_name: &str,
    target_enabled: bool,
) -> Result<(), PluginError> {
    let enabled_path = custom_nodes_path.join(plugin_name);
    let _disabled_path = custom_nodes_path
        .join(plugin_name)
        .with_extension(format!("{}{}", plugin_name, DISABLED_SUFFIX));

    // 实际上 with_extension 不对，让我用字符串拼接
    let disabled_path = custom_nodes_path.join(format!("{}{}", plugin_name, DISABLED_SUFFIX));

    // 检查当前状态
    let currently_enabled = enabled_path.exists();
    let currently_disabled = disabled_path.exists();

    if !currently_enabled && !currently_disabled {
        return Err(PluginError::NotFound(plugin_name.to_string()));
    }

    // 幂等：已是目标状态
    if target_enabled && currently_enabled {
        return Ok(());
    }
    if !target_enabled && currently_disabled {
        return Ok(());
    }

    // 执行 rename
    let (from, to) = if target_enabled {
        (disabled_path, enabled_path)
    } else {
        (enabled_path, disabled_path)
    };

    std::fs::rename(&from, &to)?;
    tracing::info!(?plugin_name, enabled = target_enabled, "plugin toggled");
    Ok(())
}

/// 获取插件目录路径
///
/// 自动识别 `.disabled` 后缀。
pub fn plugin_dir_path(custom_nodes_path: &Path, plugin_name: &str) -> Option<std::path::PathBuf> {
    let enabled = custom_nodes_path.join(plugin_name);
    if enabled.exists() {
        return Some(enabled);
    }
    let disabled = custom_nodes_path.join(format!("{}{}", plugin_name, DISABLED_SUFFIX));
    if disabled.exists() {
        return Some(disabled);
    }
    None
}

/// 从 `pyproject.toml` 或 `__init__.py` 读插件描述
fn read_description(plugin_path: &Path) -> Option<String> {
    // 优先读 pyproject.toml [project].description
    let pyproject = plugin_path.join("pyproject.toml");
    if pyproject.exists() {
        if let Ok(content) = std::fs::read_to_string(&pyproject) {
            if let Some(desc) = parse_pyproject_description(&content) {
                return Some(desc);
            }
        }
    }

    // fallback: __init__.py 的第一个 docstring
    let init_py = plugin_path.join("__init__.py");
    if init_py.exists() {
        if let Ok(content) = std::fs::read_to_string(&init_py) {
            if let Some(desc) = parse_init_docstring(&content) {
                return Some(desc);
            }
        }
    }

    None
}

/// 从 pyproject.toml 内容解析 description
///
/// 简单实现：找 `description = "..."` 行
fn parse_pyproject_description(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("description") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let rest = rest.trim();
                // 去除引号
                let desc = rest
                    .trim_matches('"')
                    .trim_matches('\'')
                    .trim();
                if !desc.is_empty() {
                    return Some(desc.to_string());
                }
            }
        }
    }
    None
}

/// 从 __init__.py 内容解析第一个 docstring
fn parse_init_docstring(content: &str) -> Option<String> {
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
    None
}

/// 读取插件安装时间（暂用目录 mtime）
///
/// 准确实现：读 .git 目录创建时间或第一次 commit 时间。
/// 本期简化：用目录 mtime。
fn read_installed_at(plugin_path: &Path) -> Option<chrono::DateTime<chrono::Utc>> {
    let metadata = std::fs::metadata(plugin_path).ok()?;
    let modified = metadata.modified().ok()?;
    Some(chrono::DateTime::<chrono::Utc>::from(modified))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_plugin_dir(parent: &Path, name: &str) -> std::path::PathBuf {
        let dir = parent.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("__init__.py"), "# test\n").unwrap();
        dir
    }

    #[test]
    fn test_scan_plugins_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_nodes = tmp.path().join("custom_nodes");
        fs::create_dir_all(&custom_nodes).unwrap();
        let plugins = scan_plugins(&custom_nodes).unwrap();
        assert!(plugins.is_empty());
    }

    #[test]
    fn test_scan_plugins_nonexistent_dir() {
        let plugins = scan_plugins(Path::new("/nonexistent/custom_nodes")).unwrap();
        assert!(plugins.is_empty());
    }

    #[test]
    fn test_scan_plugins_lists_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_nodes = tmp.path().join("custom_nodes");
        fs::create_dir_all(&custom_nodes).unwrap();

        make_plugin_dir(&custom_nodes, "plugin-a");
        make_plugin_dir(&custom_nodes, "plugin-b");

        let plugins = scan_plugins(&custom_nodes).unwrap();
        assert_eq!(plugins.len(), 2);
        assert_eq!(plugins[0].name, "plugin-a");
        assert_eq!(plugins[1].name, "plugin-b");
    }

    #[test]
    fn test_scan_plugins_skips_hidden_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_nodes = tmp.path().join("custom_nodes");
        fs::create_dir_all(&custom_nodes).unwrap();

        make_plugin_dir(&custom_nodes, "real-plugin");
        fs::create_dir_all(custom_nodes.join(".trash")).unwrap();
        fs::create_dir_all(custom_nodes.join(".cache")).unwrap();

        let plugins = scan_plugins(&custom_nodes).unwrap();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "real-plugin");
    }

    #[test]
    fn test_scan_plugins_recognizes_disabled_suffix() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_nodes = tmp.path().join("custom_nodes");
        fs::create_dir_all(&custom_nodes).unwrap();

        make_plugin_dir(&custom_nodes, "active");
        make_plugin_dir(&custom_nodes, "inactive.disabled");

        let plugins = scan_plugins(&custom_nodes).unwrap();
        assert_eq!(plugins.len(), 2);

        let active = plugins.iter().find(|p| p.name == "active").unwrap();
        assert!(active.enabled);
        assert_eq!(active.dir_name, "active");

        let inactive = plugins.iter().find(|p| p.name == "inactive").unwrap();
        assert!(!inactive.enabled);
        assert_eq!(inactive.dir_name, "inactive.disabled");
    }

    #[test]
    fn test_toggle_plugin_disables() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_nodes = tmp.path().join("custom_nodes");
        fs::create_dir_all(&custom_nodes).unwrap();
        make_plugin_dir(&custom_nodes, "toggle-me");

        toggle_plugin(&custom_nodes, "toggle-me", false).unwrap();
        assert!(custom_nodes.join("toggle-me.disabled").exists());
        assert!(!custom_nodes.join("toggle-me").exists());
    }

    #[test]
    fn test_toggle_plugin_enables() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_nodes = tmp.path().join("custom_nodes");
        fs::create_dir_all(&custom_nodes).unwrap();
        make_plugin_dir(&custom_nodes, "enable-me.disabled");

        toggle_plugin(&custom_nodes, "enable-me", true).unwrap();
        assert!(custom_nodes.join("enable-me").exists());
        assert!(!custom_nodes.join("enable-me.disabled").exists());
    }

    #[test]
    fn test_toggle_plugin_idempotent_enable() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_nodes = tmp.path().join("custom_nodes");
        fs::create_dir_all(&custom_nodes).unwrap();
        make_plugin_dir(&custom_nodes, "already-on");

        // 已启用，再启用 → 幂等 Ok
        toggle_plugin(&custom_nodes, "already-on", true).unwrap();
        assert!(custom_nodes.join("already-on").exists());
    }

    #[test]
    fn test_toggle_plugin_idempotent_disable() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_nodes = tmp.path().join("custom_nodes");
        fs::create_dir_all(&custom_nodes).unwrap();
        make_plugin_dir(&custom_nodes, "already-off.disabled");

        // 已禁用，再禁用 → 幂等 Ok
        toggle_plugin(&custom_nodes, "already-off", false).unwrap();
        assert!(custom_nodes.join("already-off.disabled").exists());
    }

    #[test]
    fn test_toggle_plugin_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_nodes = tmp.path().join("custom_nodes");
        fs::create_dir_all(&custom_nodes).unwrap();

        let result = toggle_plugin(&custom_nodes, "nonexistent", true);
        assert!(matches!(result, Err(PluginError::NotFound(_))));
    }

    #[test]
    fn test_plugin_dir_path_finds_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_nodes = tmp.path().join("custom_nodes");
        fs::create_dir_all(&custom_nodes).unwrap();
        make_plugin_dir(&custom_nodes, "test-plugin");

        let path = plugin_dir_path(&custom_nodes, "test-plugin").unwrap();
        assert!(path.ends_with("test-plugin"));
    }

    #[test]
    fn test_plugin_dir_path_finds_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_nodes = tmp.path().join("custom_nodes");
        fs::create_dir_all(&custom_nodes).unwrap();
        make_plugin_dir(&custom_nodes, "test-plugin.disabled");

        let path = plugin_dir_path(&custom_nodes, "test-plugin").unwrap();
        assert!(path.ends_with("test-plugin.disabled"));
    }

    #[test]
    fn test_plugin_dir_path_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_nodes = tmp.path().join("custom_nodes");
        fs::create_dir_all(&custom_nodes).unwrap();

        assert!(plugin_dir_path(&custom_nodes, "nonexistent").is_none());
    }

    #[test]
    fn test_parse_pyproject_description() {
        let content = r#"
[project]
name = "my-plugin"
description = "A test plugin for ComfyUI"
version = "1.0.0"
"#;
        let desc = parse_pyproject_description(content).unwrap();
        assert_eq!(desc, "A test plugin for ComfyUI");
    }

    #[test]
    fn test_parse_pyproject_description_single_quote() {
        let content = "[project]\ndescription = 'Single quoted'\n";
        let desc = parse_pyproject_description(content).unwrap();
        assert_eq!(desc, "Single quoted");
    }

    #[test]
    fn test_parse_init_docstring() {
        let content = "\"\"\"A plugin docstring.\"\"\"\n\nVERSION = '1.0'\n";
        let desc = parse_init_docstring(content).unwrap();
        assert_eq!(desc, "A plugin docstring.");
    }

    #[test]
    fn test_read_description_from_pyproject() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("pyproject.toml"),
            "[project]\ndescription = \"From pyproject\"\n",
        )
        .unwrap();
        let desc = read_description(tmp.path()).unwrap();
        assert_eq!(desc, "From pyproject");
    }

    #[test]
    fn test_read_description_falls_back_to_init_py() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("__init__.py"),
            "\"\"\"From init.\"\"\"\nVERSION = '1.0'\n",
        )
        .unwrap();
        let desc = read_description(tmp.path()).unwrap();
        // docstring 内容为 "From init."（含句号），解析后原样保留
        assert_eq!(desc, "From init.");
    }

    #[test]
    fn test_read_description_returns_none_when_no_files() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_description(tmp.path()).is_none());
    }
}
