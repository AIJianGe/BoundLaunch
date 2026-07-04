//! 路径工具
//!
//! 集中管理所有应用路径，避免散落各处
//! 详见 `PR/03-模块设计/01-Config.md` (paths 配置)

use std::path::{Path, PathBuf};

/// 应用数据目录
///
/// Windows: %APPDATA%\boundlaunch\
/// Linux:   ~/.local/share/boundlaunch/
/// macOS:   ~/Library/Application Support/boundlaunch/
pub fn app_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
        .join("boundlaunch")
}

/// config.toml 路径
pub fn config_path() -> PathBuf {
    app_data_dir().join("config.toml")
}

/// launcher.sqlite 路径（LogStore）
pub fn log_db_path() -> PathBuf {
    app_data_dir().join("launcher.sqlite")
}

/// .launcher-dirty 标记文件路径
///
/// 位于 comfyui_root 下，用于标记 torch 缺失等异常状态
pub fn dirty_marker_path(comfyui_root: &Path) -> PathBuf {
    comfyui_root.join(".launcher-dirty")
}

/// launcher 自身 PID 文件路径（用于崩溃恢复检测）
pub fn pid_file_path() -> PathBuf {
    app_data_dir().join("launcher.pid")
}

/// .trash 子目录路径（插件卸载暂存）
pub fn trash_dir(custom_nodes_dir: &Path) -> PathBuf {
    custom_nodes_dir.join(".trash")
}

/// 确保目录存在（递归创建）
pub async fn ensure_dir(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        tokio::fs::create_dir_all(path).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_paths_are_under_app_data() {
        let data = app_data_dir();
        assert!(config_path().starts_with(&data));
        assert!(log_db_path().starts_with(&data));
        assert!(pid_file_path().starts_with(&data));
    }

    #[test]
    fn test_dirty_marker_under_comfyui_root() {
        let root = Path::new("/tmp/comfyui");
        let marker = dirty_marker_path(root);
        assert_eq!(marker, Path::new("/tmp/comfyui/.launcher-dirty"));
    }
}
