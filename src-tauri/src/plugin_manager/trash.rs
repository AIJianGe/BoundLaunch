//! 回收站管理
//!
//! 设计：把卸载的插件移到 `<custom_nodes>/.trash/<plugin_name>-<timestamp>/`，
//! ComfyUI 默认不会扫描 `.trash` 隐藏目录。
//!
//! 详见 `PR/03-模块设计/04-PluginManager.md §10 卸载回收站设计`
//!
//! **v0.0.2.1**：`trash_dir` 改为本文件内部辅助函数（不再从 `common::paths` 引用）。
//! 理由：`.trash` 路径是参数化的（依赖调用方传入的 `custom_nodes_dir`），
//! 跟路径解析系统无关，留在本模块更内聚。

use std::path::{Path, PathBuf};

use crate::plugin_manager::models::{PluginError, UninstallResult};

/// `.trash` 子目录路径（参数化：基于调用方传入的 custom_nodes_dir）
///
/// v0.0.2.1：从 `common::paths::trash_dir` 迁入本模块
fn trash_dir(custom_nodes_dir: &Path) -> PathBuf {
    custom_nodes_dir.join(".trash")
}

/// 把插件目录移到回收站
///
/// 流程：
/// 1. 确保 `<custom_nodes>/.trash/` 存在
/// 2. 重命名 `<custom_nodes>/<plugin_dir>` → `<custom_nodes>/.trash/<plugin_name>-<ts>/`
///
/// 失败情况：
/// - 回收站目录创建失败 → `TrashCreateFailed`
/// - 重命名失败 → `IoError`（可能是文件占用 / 跨设备）
pub fn move_to_trash(
    plugin_path: &Path,
    custom_nodes_dir: &Path,
) -> Result<UninstallResult, PluginError> {
    let trash_root = trash_dir(custom_nodes_dir);
    if !trash_root.exists() {
        std::fs::create_dir_all(&trash_root)
            .map_err(|_| PluginError::TrashCreateFailed(trash_root.clone()))?;
    }

    let plugin_name = plugin_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let target = trash_root.join(format!("{}-{}", plugin_name, ts));

    std::fs::rename(plugin_path, &target)?;

    tracing::info!(?plugin_path, ?target, "plugin moved to trash");
    Ok(UninstallResult {
        moved_to: target,
        recoverable: true,
    })
}

/// 从回收站恢复插件（手动恢复接口）
///
/// 把 `<trash>/<plugin_name>-<ts>/` 移回 `<custom_nodes>/<plugin_name>/`
///
/// 若 `<custom_nodes>/<plugin_name>/` 已存在（用户已重装），返回 `AlreadyExists`。
pub fn restore_from_trash(
    trash_entry: &Path,
    custom_nodes_dir: &Path,
    plugin_name: &str,
) -> Result<PathBuf, PluginError> {
    let target = custom_nodes_dir.join(plugin_name);
    if target.exists() {
        return Err(PluginError::AlreadyExists(plugin_name.to_string()));
    }
    std::fs::rename(trash_entry, &target)?;
    tracing::info!(?trash_entry, ?target, "plugin restored from trash");
    Ok(target)
}

/// 列出回收站中的所有插件备份
pub fn list_trash(custom_nodes_dir: &Path) -> Vec<PathBuf> {
    let trash_root = trash_dir(custom_nodes_dir);
    let entries = match std::fs::read_dir(&trash_root) {
        Ok(e) => e,
        Err(_) => return vec![],
    };
    let mut result: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    result.sort();
    result
}

/// 清空回收站
pub fn clear_trash(custom_nodes_dir: &Path) -> Result<usize, PluginError> {
    let trash_root = trash_dir(custom_nodes_dir);
    if !trash_root.exists() {
        return Ok(0);
    }
    let mut count = 0;
    for entry in std::fs::read_dir(&trash_root)?.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
            count += 1;
        }
    }
    tracing::info!(?trash_root, removed = count, "trash cleared");
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_plugin_dir(parent: &Path, name: &str) -> PathBuf {
        let dir = parent.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("__init__.py"), "# test plugin\n").unwrap();
        dir
    }

    #[test]
    fn test_move_to_trash_creates_trash_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_nodes = tmp.path().join("custom_nodes");
        fs::create_dir_all(&custom_nodes).unwrap();
        let plugin = make_plugin_dir(&custom_nodes, "my-plugin");

        let result = move_to_trash(&plugin, &custom_nodes).unwrap();
        assert!(result.recoverable);
        assert!(!plugin.exists(), "原插件目录应已被移走");
        assert!(result.moved_to.exists(), "回收站应有副本");
        assert!(result.moved_to.to_string_lossy().contains("my-plugin-"));
    }

    #[test]
    fn test_move_to_trash_creates_trash_root_if_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_nodes = tmp.path().join("custom_nodes");
        fs::create_dir_all(&custom_nodes).unwrap();
        let plugin = make_plugin_dir(&custom_nodes, "test-plugin");

        // .trash 目录初始不存在
        let trash_root = custom_nodes.join(".trash");
        assert!(!trash_root.exists());

        move_to_trash(&plugin, &custom_nodes).unwrap();
        assert!(trash_root.exists(), ".trash 目录应被自动创建");
    }

    #[test]
    fn test_restore_from_trash_moves_back() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_nodes = tmp.path().join("custom_nodes");
        fs::create_dir_all(&custom_nodes).unwrap();
        let plugin = make_plugin_dir(&custom_nodes, "restore-me");

        let result = move_to_trash(&plugin, &custom_nodes).unwrap();
        let restored = restore_from_trash(&result.moved_to, &custom_nodes, "restore-me").unwrap();
        assert!(restored.exists());
        assert!(!result.moved_to.exists());
    }

    #[test]
    fn test_restore_from_trash_already_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_nodes = tmp.path().join("custom_nodes");
        fs::create_dir_all(&custom_nodes).unwrap();
        let plugin = make_plugin_dir(&custom_nodes, "exists");
        let result = move_to_trash(&plugin, &custom_nodes).unwrap();

        // 重新创建同名插件
        let _new_plugin = make_plugin_dir(&custom_nodes, "exists");

        let restore_result = restore_from_trash(&result.moved_to, &custom_nodes, "exists");
        assert!(matches!(restore_result, Err(PluginError::AlreadyExists(_))));
    }

    #[test]
    fn test_list_trash_returns_all_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_nodes = tmp.path().join("custom_nodes");
        fs::create_dir_all(&custom_nodes).unwrap();

        let p1 = make_plugin_dir(&custom_nodes, "p1");
        let p2 = make_plugin_dir(&custom_nodes, "p2");
        move_to_trash(&p1, &custom_nodes).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        move_to_trash(&p2, &custom_nodes).unwrap();

        let trash_entries = list_trash(&custom_nodes);
        assert_eq!(trash_entries.len(), 2);
    }

    #[test]
    fn test_list_trash_empty_when_no_trash_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_nodes = tmp.path().join("custom_nodes");
        fs::create_dir_all(&custom_nodes).unwrap();
        assert!(list_trash(&custom_nodes).is_empty());
    }

    #[test]
    fn test_clear_trash_removes_all() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_nodes = tmp.path().join("custom_nodes");
        fs::create_dir_all(&custom_nodes).unwrap();

        let p1 = make_plugin_dir(&custom_nodes, "p1");
        let p2 = make_plugin_dir(&custom_nodes, "p2");
        move_to_trash(&p1, &custom_nodes).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        move_to_trash(&p2, &custom_nodes).unwrap();

        let removed = clear_trash(&custom_nodes).unwrap();
        assert_eq!(removed, 2);
        assert!(list_trash(&custom_nodes).is_empty());
    }

    #[test]
    fn test_clear_trash_no_trash_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let custom_nodes = tmp.path().join("custom_nodes");
        fs::create_dir_all(&custom_nodes).unwrap();
        let removed = clear_trash(&custom_nodes).unwrap();
        assert_eq!(removed, 0);
    }
}
