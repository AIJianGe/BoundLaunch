//! 原子写 + 文件权限保护
//!
//! 详见 `PR/03-模块设计/01-Config.md §10 文件权限保护`
//!
//! 策略：
//! 1. 写到临时文件 `*.toml.tmp`
//! 2. Unix 设置 0600 权限（防其他用户读取）
//! 3. rename 到目标路径（原子操作）
//! 4. Windows 依赖 %APPDATA% 目录的默认 ACL

use std::io;
use std::path::Path;
use tokio::fs;

/// 原子写入文件内容
///
/// 写到 tmp 文件 → 设置权限 → rename
pub async fn atomic_write(path: &Path, content: &str) -> io::Result<()> {
    let tmp = path.with_extension("toml.tmp");

    // 1. 写到临时文件
    fs::write(&tmp, content).await?;

    // 2. Unix 设置 0600 权限
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&tmp).await?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&tmp, perms).await?;
    }
    // Windows: 依赖 %APPDATA% 目录的默认 ACL，无需显式设置

    // 3. 原子 rename
    fs::rename(&tmp, path).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_atomic_write_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.toml");
        atomic_write(&path, "key = \"value\"\n").await.unwrap();

        let content = fs::read_to_string(&path).await.unwrap();
        assert_eq!(content, "key = \"value\"\n");
    }

    #[tokio::test]
    async fn test_atomic_write_replaces_existing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.toml");
        atomic_write(&path, "old = true\n").await.unwrap();
        atomic_write(&path, "new = true\n").await.unwrap();

        let content = fs::read_to_string(&path).await.unwrap();
        assert_eq!(content, "new = true\n");
    }
}
