//! Updater 路径常量
//!
//! 所有 staging / pending / cache 目录都基于 `env_root`（绿色版根目录）解析
//!
//! ## 目录结构
//!
//! ```text
//! <env_root>/
///   BoundLaunch.exe
///   BoundLaunch.exe.new         ← 更新挂起（重启时 rename）
///   BoundLaunch.dll
///   BoundLaunch.dll.new         ← 更新挂起（重启时 rename）
///   resources/
///     uv/
///       uv-x86_64-pc-windows-msvc.exe
///   .boundlaunch/
///     update-staging/
///       v0.0.2/                 ← 下载 + 解压到此
///         BoundLaunch.exe
///         BoundLaunch.dll
///         resources/uv/...
///     update-pending/           ← "重启时应用更新" 标志目录
///       BoundLaunch.exe.new
///       BoundLaunch.dll.new
///       resources/uv/...
/// ```

use std::path::PathBuf;

use crate::paths::env_paths;

/// staging 根目录：下载的 zip 解压到这里
pub fn staging_dir(version: &str) -> PathBuf {
    let env = env_paths::resolve().expect("env_paths::resolve failed");
    env.boundlaunch_data_dir.join("update-staging").join(version)
}

/// pending 目录：staging 处理后（白名单拷贝）放到这里，等下次启动 rename
pub fn pending_dir() -> PathBuf {
    let env = env_paths::resolve().expect("env_paths::resolve failed");
    env.boundlaunch_data_dir.join("update-pending")
}

/// 当前应用的 exe 同级目录
pub fn env_root() -> PathBuf {
    let env = env_paths::resolve().expect("env_paths::resolve failed");
    env.env_root
}

/// 用于清理 staging 目录的辅助函数
pub fn cleanup_staging() {
    let env = env_paths::resolve().expect("env_paths::resolve failed");
    let staging_root = env.boundlaunch_data_dir.join("update-staging");
    if staging_root.exists() {
        if let Err(e) = std::fs::remove_dir_all(&staging_root) {
            tracing::warn!(error = %e, "failed to cleanup update-staging");
        } else {
            tracing::info!("update-staging cleaned up");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_paths_under_env_root() {
        let root = env_root();
        let staging = staging_dir("0.0.2");
        let pending = pending_dir();
        // 所有路径都基于 env_root
        assert!(staging.starts_with(&root));
        assert!(pending.starts_with(&root));
        // staging 在 .boundlaunch/update-staging/<version>/
        assert!(staging.to_string_lossy().contains("update-staging"));
        // pending 在 .boundlaunch/update-pending/
        assert!(pending.to_string_lossy().contains("update-pending"));
    }
}
