//! 仓库地址切换与备份恢复（F31）
//!
//! ## 设计模式
//! - **Template Method**：固定步骤（备份→更新Config→克隆→迁移→重建链接→失效缓存）
//! - **Rollback**：克隆失败时恢复备份 + 恢复 Config
//!
//! ## 备份策略
//! - 备份根目录 = `{comfyui_root_parent}/Backup/`
//! - 备份子目录 = `ComfyUI_bak01`、`ComfyUI_bak02`...（两位补零，最多 5 个）
//! - 超过 5 个时删除编号最小的（最旧）
//! - 每个备份含 `.backup_meta.json` 元信息
//!
//! ## 切换流程
//! 1. 校验 URL（支持带 token 的 GitHub URL）
//! 2. 备份当前 ComfyUI → rename 到 Backup/ComfyUI_bakNN
//! 3. 更新 Config.paths.comfyui_repo_url
//! 4. 克隆新仓库
//! 5. 可选：迁移 custom_nodes
//! 6. 重建 models 软链接
//! 7. 失效 tags 缓存
//!
//! 详见 `PR/03-模块设计/03-CoreManager.md §5.1 仓库地址切换流程`

use std::path::{Path, PathBuf};

use chrono::Utc;
use once_cell::sync::Lazy;
use regex::Regex;

use crate::core_manager::git_ops;
use crate::core_manager::models::{
    BackupInfo, BackupMeta, SwitchRepoResult, COMFYUI_REPO_URL,
};
use crate::error::CoreError;

/// 备份保留上限
const MAX_BACKUPS: usize = 5;

/// 备份目录名前缀
const BACKUP_PREFIX: &str = "ComfyUI_bak";

/// 备份元信息文件名
const BACKUP_META_FILE: &str = ".backup_meta.json";

/// 备份根目录名（在 comfyui_root 父目录下）
const BACKUP_ROOT_DIR: &str = "Backup";

/// GitHub URL 校验正则（支持带 token 的私有仓库）
///
/// 匹配格式：
/// - `https://github.com/owner/repo`
/// - `https://github.com/owner/repo.git`
/// - `https://token@github.com/owner/repo.git`
/// - `https://user:token@github.com/owner/repo.git`
static GITHUB_URL_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^https://([^@/]*@)?github\.com/[^/]+/[^/]+(\.git)?$")
        .expect("invalid regex")
});

/// 校验 GitHub URL（支持公开/私有仓库）
pub fn validate_github_url(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err("仅支持 HTTPS 协议".into());
    }
    if !GITHUB_URL_RE.is_match(url) {
        return Err("URL 格式不正确，应为 https://github.com/owner/repo".into());
    }
    Ok(())
}

/// URL 脱敏：把认证信息中的 token 部分替换为 ***
pub fn mask_url_credentials(url: &str) -> String {
    // 检查是否有认证信息（@ 前面部分）
    if let Some(at_pos) = url.find('@') {
        let https_len = "https://".len();
        if at_pos > https_len {
            // 有认证信息
            let after_at = &url[at_pos + 1..];
            return format!("https://***@{}", after_at);
        }
    }
    url.to_string()
}

/// 获取备份根目录路径
///
/// `{comfyui_root_parent}/Backup/`
fn backup_root_dir(comfyui_root: &Path) -> PathBuf {
    comfyui_root
        .parent()
        .unwrap_or(Path::new("."))
        .join(BACKUP_ROOT_DIR)
}

/// 扫描备份目录，返回所有备份的编号（已排序）
///
/// 返回值：(编号, 目录名) 列表，按编号升序
fn scan_existing_backups(backup_root: &Path) -> Vec<(usize, String)> {
    let mut result = Vec::new();
    if let Ok(entries) = std::fs::read_dir(backup_root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(num_str) = name.strip_prefix(BACKUP_PREFIX) {
                if let Ok(num) = num_str.parse::<usize>() {
                    result.push((num, name));
                }
            }
        }
    }
    result.sort_by_key(|(n, _)| *n);
    result
}

/// 计算下一个备份编号和目录名
///
/// 逻辑：
/// 1. 扫描现有备份
/// 2. 如果已达 MAX_BACKUPS，删除编号最小的
/// 3. 返回最大编号 + 1
fn next_backup_slot(backup_root: &Path) -> Result<(usize, String), CoreError> {
    let existing = scan_existing_backups(backup_root);

    // 删除超限的旧备份
    if existing.len() >= MAX_BACKUPS {
        let to_remove = existing.len() - MAX_BACKUPS + 1;
        for i in 0..to_remove {
            let (_, name) = &existing[i];
            let path = backup_root.join(name);
            tracing::info!(?path, "removing old backup (exceeded max)");
            std::fs::remove_dir_all(&path).map_err(|e| {
                CoreError::GitError(format!("failed to remove old backup {:?}: {}", path, e))
            })?;
        }
        // 返回剩余的重新扫描
        let remaining = scan_existing_backups(backup_root);
        let max_num = remaining.last().map(|(n, _)| *n).unwrap_or(0);
        let next_num = max_num + 1;
        return Ok((next_num, format!("{}{:02}", BACKUP_PREFIX, next_num)));
    }

    let max_num = existing.last().map(|(n, _)| *n).unwrap_or(0);
    let next_num = max_num + 1;
    Ok((next_num, format!("{}{:02}", BACKUP_PREFIX, next_num)))
}

/// 读取备份元信息
pub fn read_backup_meta(backup_path: &Path) -> Option<BackupMeta> {
    let meta_path = backup_path.join(BACKUP_META_FILE);
    let content = std::fs::read_to_string(&meta_path).ok()?;
    serde_json::from_str(&content).ok()
}

/// 写入备份元信息
fn write_backup_meta(backup_path: &Path, meta: &BackupMeta) -> Result<(), CoreError> {
    let meta_path = backup_path.join(BACKUP_META_FILE);
    let json = serde_json::to_string_pretty(meta)
        .map_err(|e| CoreError::GitError(format!("serialize backup meta failed: {}", e)))?;
    std::fs::write(&meta_path, json)
        .map_err(|e| CoreError::GitError(format!("write backup meta failed: {}", e)))?;
    Ok(())
}

/// 计算目录大小（递归，字节）
fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

/// 统计 custom_nodes 目录下的插件数量
fn count_custom_nodes(comfyui_root: &Path) -> usize {
    let cn_path = comfyui_root.join("custom_nodes");
    if !cn_path.exists() {
        return 0;
    }
    std::fs::read_dir(&cn_path)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .count()
        })
        .unwrap_or(0)
}

/// 列出所有备份信息
///
/// 扫描 `{comfyui_root_parent}/Backup/` 目录，返回 BackupInfo 列表（按编号降序）
pub fn list_backups(comfyui_root: &Path) -> Result<Vec<BackupInfo>, CoreError> {
    let backup_root = backup_root_dir(comfyui_root);
    if !backup_root.exists() {
        return Ok(Vec::new());
    }

    let existing = scan_existing_backups(&backup_root);
    let mut result = Vec::new();

    for (_, name) in existing.iter().rev() {
        // 降序
        let backup_path = backup_root.join(name);
        let meta = read_backup_meta(&backup_path);

        let info = BackupInfo {
            name: name.clone(),
            path: backup_path.to_string_lossy().to_string(),
            backed_up_at: meta
                .as_ref()
                .map(|m| m.backed_up_at.to_rfc3339())
                .unwrap_or_else(|| "unknown".to_string()),
            repo_url_masked: meta
                .as_ref()
                .map(|m| m.repo_url_masked.clone())
                .unwrap_or_else(|| "unknown".to_string()),
            current_tag: meta.as_ref().and_then(|m| m.current_tag.clone()),
            current_commit: meta
                .as_ref()
                .map(|m| m.current_commit.clone())
                .unwrap_or_default(),
            size_bytes: dir_size(&backup_path),
        };
        result.push(info);
    }

    Ok(result)
}

/// 备份当前 ComfyUI 目录
///
/// 1. 创建 Backup/ 目录（如不存在）
/// 2. 计算下一个备份编号
/// 3. rename comfyui_root → Backup/ComfyUI_bakNN
/// 4. 写入 .backup_meta.json
///
/// 返回备份目录名
fn backup_current_comfyui(
    comfyui_root: &Path,
    repo_url: &str,
    current_tag: Option<&str>,
    current_commit: &str,
) -> Result<String, CoreError> {
    let backup_root = backup_root_dir(comfyui_root);

    // 创建 Backup/ 目录
    std::fs::create_dir_all(&backup_root)
        .map_err(|e| CoreError::GitError(format!("create backup dir failed: {}", e)))?;

    let (_, backup_name) = next_backup_slot(&backup_root)?;
    let backup_path = backup_root.join(&backup_name);

    tracing::info!(
        from = ?comfyui_root,
        to = ?backup_path,
        "backing up ComfyUI"
    );

    // rename（同盘原子操作，秒级）
    std::fs::rename(comfyui_root, &backup_path).map_err(|e| {
        CoreError::GitError(format!(
            "backup rename failed ({} -> {}): {}",
            comfyui_root.display(),
            backup_path.display(),
            e
        ))
    })?;

    // 写入元信息
    let custom_nodes_count = count_custom_nodes(&backup_path);
    let meta = BackupMeta {
        backed_up_at: Utc::now(),
        repo_url: repo_url.to_string(),
        repo_url_masked: mask_url_credentials(repo_url),
        current_tag: current_tag.map(|s| s.to_string()),
        current_commit: current_commit.to_string(),
        comfyui_root_at_backup: comfyui_root.to_string_lossy().to_string(),
        custom_nodes_count,
    };
    write_backup_meta(&backup_path, &meta)?;

    Ok(backup_name)
}

/// 从备份恢复
///
/// 1. 当前 ComfyUI 也备份（保护当前状态）
/// 2. rename 备份目录 → comfyui_root
/// 3. 读取备份元信息，返回 URL
pub fn restore_from_backup(
    comfyui_root: &Path,
    backup_name: &str,
) -> Result<(String, String), CoreError> {
    let backup_root = backup_root_dir(comfyui_root);
    let backup_path = backup_root.join(backup_name);

    if !backup_path.exists() {
        return Err(CoreError::GitError(format!(
            "backup not found: {}",
            backup_path.display()
        )));
    }

    // 读取备份元信息
    let meta = read_backup_meta(&backup_path).ok_or_else(|| {
        CoreError::GitError("backup meta not found or invalid".to_string())
    })?;

    // 如果当前 ComfyUI 存在，也备份它（保护当前状态）
    if comfyui_root.exists() {
        let (_, current_backup_name) = next_backup_slot(&backup_root)?;
        let current_backup_path = backup_root.join(&current_backup_name);

        tracing::info!(
            from = ?comfyui_root,
            to = ?current_backup_path,
            "backing up current ComfyUI before restore"
        );

        std::fs::rename(comfyui_root, &current_backup_path).map_err(|e| {
            CoreError::GitError(format!("backup current before restore failed: {}", e))
        })?;

        // 写入当前状态的元信息
        let current_meta = BackupMeta {
            backed_up_at: Utc::now(),
            repo_url: "current_state".to_string(),
            repo_url_masked: "current_state".to_string(),
            current_tag: None,
            current_commit: String::new(),
            comfyui_root_at_backup: comfyui_root.to_string_lossy().to_string(),
            custom_nodes_count: 0,
        };
        write_backup_meta(&current_backup_path, &current_meta)?;
    }

    // 恢复备份
    tracing::info!(
        from = ?backup_path,
        to = ?comfyui_root,
        "restoring backup"
    );

    std::fs::rename(&backup_path, comfyui_root).map_err(|e| {
        CoreError::GitError(format!(
            "restore rename failed ({} -> {}): {}",
            backup_path.display(),
            comfyui_root.display(),
            e
        ))
    })?;

    Ok((meta.repo_url, meta.repo_url_masked))
}

/// 迁移 custom_nodes 从备份到新仓库
///
/// 从 backup_path/custom_nodes 复制到 comfyui_root/custom_nodes
fn migrate_custom_nodes(
    backup_path: &Path,
    comfyui_root: &Path,
) -> Result<usize, CoreError> {
    let src = backup_path.join("custom_nodes");
    if !src.exists() {
        return Ok(0);
    }

    let dst = comfyui_root.join("custom_nodes");
    std::fs::create_dir_all(&dst)
        .map_err(|e| CoreError::GitError(format!("create custom_nodes dir failed: {}", e)))?;

    let mut count = 0;
    copy_dir_recursive(&src, &dst, &mut count)?;
    tracing::info!(migrated = count, "migrated custom_nodes");
    Ok(count)
}

/// 递归复制目录
fn copy_dir_recursive(src: &Path, dst: &Path, count: &mut usize) -> Result<(), CoreError> {
    std::fs::create_dir_all(dst)
        .map_err(|e| CoreError::GitError(format!("mkdir {:?} failed: {}", dst, e)))?;

    for entry in std::fs::read_dir(src)
        .map_err(|e| CoreError::GitError(format!("read_dir {:?} failed: {}", src, e)))?
    {
        let entry = entry.map_err(|e| CoreError::GitError(format!("dir entry failed: {}", e)))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        // 跳过 .git 目录（custom_nodes 内的 .git 会导致问题）
        if entry.file_name() == ".git" {
            continue;
        }

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path, count)?;
        } else {
            std::fs::copy(&src_path, &dst_path).map_err(|e| {
                CoreError::GitError(format!("copy {:?} -> {:?} failed: {}", src_path, dst_path, e))
            })?;
        }
    }

    *count += 1;
    Ok(())
}

/// 删除目录（容错，删除失败只 warn 不报错）
fn remove_dir_safe(path: &Path) {
    if path.exists() {
        if let Err(e) = std::fs::remove_dir_all(path) {
            tracing::warn!(?path, error = %e, "failed to remove dir");
        }
    }
}

/// 执行仓库地址切换（同步，内部都是同步 git2 操作）
///
/// 完整流程：
/// 1. 备份当前 ComfyUI
/// 2. 克隆新仓库
/// 3. 可选迁移 custom_nodes
///
/// 注意：Config 更新和缓存失效由调用方（mod.rs）负责
pub fn switch_repo_url_sync(
    comfyui_root: &Path,
    old_url: &str,
    new_url: &str,
    migrate_custom_nodes_flag: bool,
) -> Result<SwitchRepoResult, CoreError> {
    let new_url = new_url.to_string();
    let old_url = old_url.to_string();

    // 校验 URL
    if let Err(e) = validate_github_url(&new_url) {
        return Ok(SwitchRepoResult::RolledBack {
            to_url: old_url,
            error: e,
            rollback_clean: true,
        });
    }

    // 如果当前 ComfyUI 不存在（首次克隆场景），无需备份
    let need_backup = comfyui_root.exists() && comfyui_root.join(".git").exists();

    // 获取当前版本信息（用于备份元信息）
    let (current_tag, current_commit) = if need_backup {
        match git_ops::open_repo(comfyui_root) {
            Ok(repo) => {
                let tag = git_ops::current_tag(&repo).unwrap_or(None);
                let commit = git_ops::current_commit(&repo).unwrap_or_default();
                (tag, commit)
            }
            Err(_) => (None, String::new()),
        }
    } else {
        (None, String::new())
    };

    // 步骤 1：备份当前 ComfyUI
    let backup_name = if need_backup {
        match backup_current_comfyui(
            comfyui_root,
            &old_url,
            current_tag.as_deref(),
            &current_commit,
        ) {
            Ok(name) => Some(name),
            Err(e) => {
                return Ok(SwitchRepoResult::RolledBack {
                    to_url: old_url,
                    error: format!("备份失败: {}", e),
                    rollback_clean: true,
                });
            }
        }
    } else {
        None
    };

    // 获取备份路径（用于后续迁移和回滚）
    let backup_path = backup_name.as_ref().map(|name| {
        backup_root_dir(comfyui_root).join(name)
    });

    // 步骤 2：克隆新仓库
    let clone_start = std::time::Instant::now();
    let clone_result = git_ops::clone_repo(comfyui_root, &new_url);
    let clone_elapsed_ms = clone_start.elapsed().as_millis() as u64;

    if let Err(e) = clone_result {
        // 克隆失败 → 回滚
        tracing::error!(error = %e, "clone failed, rolling back");

        // 删除半成品
        remove_dir_safe(comfyui_root);

        // 恢复备份
        let rollback_clean = if let Some(ref bp) = backup_path {
            match std::fs::rename(bp, comfyui_root) {
                Ok(()) => {
                    tracing::info!("restored backup after clone failure");
                    true
                }
                Err(e2) => {
                    tracing::error!(error = %e2, "failed to restore backup");
                    false
                }
            }
        } else {
            true // 无备份需恢复
        };

        return Ok(SwitchRepoResult::RolledBack {
            to_url: old_url,
            error: format!("克隆失败: {}", e),
            rollback_clean,
        });
    }

    // 步骤 3：可选迁移 custom_nodes
    if migrate_custom_nodes_flag {
        if let Some(ref bp) = backup_path {
            if let Err(e) = migrate_custom_nodes(bp, comfyui_root) {
                tracing::warn!(error = %e, "custom_nodes migration failed (non-fatal)");
            }
        }
    }

    // 步骤 4：重建 models 软链接（在外部调用方执行，因为需要 ConfigService）
    // 此处仅返回成功，调用方负责重建链接和失效缓存

    let masked_new = mask_url_credentials(&new_url);
    let masked_old = mask_url_credentials(&old_url);
    tracing::info!(
        from = %masked_old,
        to = %masked_new,
        backup = ?backup_name,
        elapsed_ms = clone_elapsed_ms,
        "repo URL switched successfully"
    );

    Ok(SwitchRepoResult::Success {
        from_url: masked_old,
        to_url: masked_new,
        backup_name,
        clone_elapsed_ms,
    })
}

/// 执行备份恢复（同步）
///
/// 1. 当前 ComfyUI 也备份
/// 2. rename 备份 → comfyui_root
/// 3. 返回备份的 URL（调用方写回 Config）
pub fn restore_backup_sync(
    comfyui_root: &Path,
    backup_name: &str,
) -> Result<(String, String), CoreError> {
    restore_from_backup(comfyui_root, backup_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_github_url_public() {
        assert!(validate_github_url("https://github.com/Comfy-Org/ComfyUI.git").is_ok());
        assert!(validate_github_url("https://github.com/Comfy-Org/ComfyUI").is_ok());
        assert!(validate_github_url("https://github.com/comfyanonymous/ComfyUI.git").is_ok());
    }

    #[test]
    fn test_validate_github_url_private() {
        assert!(validate_github_url("https://token@github.com/owner/repo.git").is_ok());
        assert!(validate_github_url("https://user:token@github.com/owner/repo.git").is_ok());
    }

    #[test]
    fn test_validate_github_url_rejects_invalid() {
        assert!(validate_github_url("http://github.com/owner/repo").is_err());
        assert!(validate_github_url("https://gitlab.com/owner/repo.git").is_err());
        assert!(validate_github_url("https://github.com/owner").is_err());
        assert!(validate_github_url("not a url").is_err());
    }

    #[test]
    fn test_mask_url_no_credentials() {
        let url = "https://github.com/Comfy-Org/ComfyUI.git";
        assert_eq!(mask_url_credentials(url), url);
    }

    #[test]
    fn test_mask_url_with_token() {
        let url = "https://ghp_abc123@github.com/owner/repo.git";
        assert_eq!(mask_url_credentials(url), "https://***@github.com/owner/repo.git");
    }

    #[test]
    fn test_mask_url_with_user_token() {
        let url = "https://user:ghp_abc123@github.com/owner/repo.git";
        assert_eq!(mask_url_credentials(url), "https://***@github.com/owner/repo.git");
    }

    #[test]
    fn test_next_backup_slot_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let backup_root = tmp.path().join("Backup");
        std::fs::create_dir_all(&backup_root).unwrap();
        let (num, name) = next_backup_slot(&backup_root).unwrap();
        assert_eq!(num, 1);
        assert_eq!(name, "ComfyUI_bak01");
    }

    #[test]
    fn test_next_backup_slot_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let backup_root = tmp.path().join("Backup");
        std::fs::create_dir_all(&backup_root).unwrap();
        std::fs::create_dir_all(backup_root.join("ComfyUI_bak01")).unwrap();
        std::fs::create_dir_all(backup_root.join("ComfyUI_bak02")).unwrap();
        let (num, name) = next_backup_slot(&backup_root).unwrap();
        assert_eq!(num, 3);
        assert_eq!(name, "ComfyUI_bak03");
    }

    #[test]
    fn test_next_backup_slot_overflow() {
        let tmp = tempfile::tempdir().unwrap();
        let backup_root = tmp.path().join("Backup");
        std::fs::create_dir_all(&backup_root).unwrap();
        for i in 1..=MAX_BACKUPS {
            std::fs::create_dir_all(backup_root.join(format!("ComfyUI_bak{:02}", i))).unwrap();
        }
        let (num, name) = next_backup_slot(&backup_root).unwrap();
        assert_eq!(num, MAX_BACKUPS + 1);
        assert_eq!(name, format!("ComfyUI_bak{:02}", MAX_BACKUPS + 1));
        // 最旧的应已被删除
        assert!(!backup_root.join("ComfyUI_bak01").exists());
    }
}
