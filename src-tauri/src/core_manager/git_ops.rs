//! git2 操作封装
//!
//! 设计模式：**Adapter** - 将 libgit2 C 库封装为 Rust 异步接口
//!
//! 详见 `PR/03-模块设计/03-CoreManager.md §5 数据流`
//!
//! 关键约束：
//! - 所有 git2 调用是同步阻塞，必须用 `tokio::task::spawn_blocking` 包裹
//! - 所有操作通过 `repo_lock` 串行化（mod.rs 层保证）

use std::path::{Path, PathBuf};

use git2::{Repository, Signature};
use chrono::Utc;

use crate::error::CoreError;

use super::models::{CheckoutResult, TagInfo};

/// 打开仓库
///
/// 不存在则返回 NotCloned
pub fn open_repo(repo_path: &Path) -> Result<Repository, CoreError> {
    if !repo_path.exists() || !repo_path.join(".git").exists() {
        return Err(CoreError::NotCloned);
    }
    Repository::open(repo_path).map_err(|e| CoreError::GitError(e.to_string()))
}

/// 列出所有 tag
///
/// 返回按 tag 名倒序排列的 TagInfo 列表
pub fn list_tags(repo: &Repository) -> Result<Vec<TagInfo>, CoreError> {
    let tags = repo
        .tag_names(None)
        .map_err(|e| CoreError::GitError(e.to_string()))?;

    let mut result = Vec::new();
    for tag_name in tags.iter().flatten() {
        let name = tag_name.to_string();
        let ref_name = format!("refs/tags/{}", name);

        // 解析 tag 引用
        let commit = repo
            .revparse_single(&ref_name)
            .ok()
            .map(|obj| obj.id().to_string())
            .unwrap_or_default();

        // 解析提交时间
        let date = repo
            .revparse_single(&ref_name)
            .and_then(|obj| obj.peel_to_commit())
            .ok()
            .and_then(|c| {
                let ts = c.time();
                chrono::DateTime::from_timestamp(ts.seconds(), 0)
            })
            .unwrap_or_else(Utc::now);

        let is_stable = super::tags::is_stable_tag(&name);

        result.push(TagInfo {
            name,
            is_stable,
            commit,
            date,
        });
    }

    // 按 tag 名倒序（v0.3.10 在 v0.3.9 前）
    result.sort_by(|a, b| b.name.cmp(&a.name));

    Ok(result)
}

/// 当前 HEAD 对应 tag
pub fn current_tag(repo: &Repository) -> Result<Option<String>, CoreError> {
    let head = repo
        .head()
        .map_err(|e| CoreError::GitError(e.to_string()))?;

    let target_commit = head
        .peel_to_commit()
        .map_err(|e| CoreError::GitError(e.to_string()))?;

    let target_id = target_commit.id();

    // 遍历所有 tag，找到指向同一 commit 的
    let tags = repo
        .tag_names(None)
        .map_err(|e| CoreError::GitError(e.to_string()))?;

    for tag_name in tags.iter().flatten() {
        let ref_name = format!("refs/tags/{}", tag_name);
        if let Ok(obj) = repo.revparse_single(&ref_name) {
            if let Ok(commit) = obj.peel_to_commit() {
                if commit.id() == target_id {
                    return Ok(Some(tag_name.to_string()));
                }
            }
        }
    }

    Ok(None)
}

/// 当前 HEAD commit hash
pub fn current_commit(repo: &Repository) -> Result<String, CoreError> {
    let head = repo
        .head()
        .map_err(|e| CoreError::GitError(e.to_string()))?;

    let commit = head
        .peel_to_commit()
        .map_err(|e| CoreError::GitError(e.to_string()))?;

    Ok(commit.id().to_string())
}

/// 检查工作区是否有未提交改动
pub fn has_local_changes(repo: &Repository) -> Result<bool, CoreError> {
    let mut status_opts = git2::StatusOptions::new();
    status_opts.include_untracked(true);

    let statuses = repo
        .statuses(Some(&mut status_opts))
        .map_err(|e| CoreError::GitError(e.to_string()))?;

    Ok(!statuses.is_empty())
}

/// 切换到指定 tag
///
/// - 若已在目标 tag → 返回 AlreadyOnTag
/// - 若有本地改动 → stash 后切换，返回 StashedAndSwitched
/// - 否则 → 切换并返回 Switched
pub fn checkout_tag(repo: &mut Repository, tag: &str) -> Result<CheckoutResult, CoreError> {
    use super::models::CheckoutResult;

    let from = current_tag(repo)?;

    // 已在目标 tag
    if from.as_deref() == Some(tag) {
        return Ok(CheckoutResult::AlreadyOnTag(tag.to_string()));
    }

    let has_changes = has_local_changes(repo)?;
    let mut stash_ref = String::new();

    if has_changes {
        // stash 用户改动
        let signature = Signature::now("launcher", "launcher@local")
            .map_err(|e| CoreError::GitError(e.to_string()))?;

        let message = format!(
            "{}-{}-{}-{}",
            super::models::AUTO_STASH_PREFIX,
            Utc::now().timestamp(),
            from.as_deref().unwrap_or("unknown"),
            tag
        );

        // stash 包含 untracked 文件
        let mut stash_opts = git2::StashFlags::default();
        stash_opts.insert(git2::StashFlags::INCLUDE_UNTRACKED);

        repo.stash_save2(
            &signature,
            Some(&message),
            Some(stash_opts),
        )
        .map_err(|e| CoreError::GitError(e.to_string()))?;

        stash_ref = message;
        tracing::info!(stash = %stash_ref, "stashed local changes before checkout");
    }

    // checkout tag
    let ref_name = format!("refs/tags/{}", tag);
    let obj = repo
        .revparse_single(&ref_name)
        .map_err(|e| CoreError::GitError(format!("tag {} not found: {}", tag, e)))?;

    repo.checkout_tree(&obj, None)
        .map_err(|e| CoreError::GitError(e.to_string()))?;

    // 设置当前分支为 launcher-current
    repo.set_head("HEAD")
        .map_err(|e| CoreError::GitError(e.to_string()))?;

    let from_str = from.clone().unwrap_or_else(|| "unknown".to_string());

    if !stash_ref.is_empty() {
        Ok(CheckoutResult::StashedAndSwitched {
            stash_ref,
            from: from_str,
            to: tag.to_string(),
        })
    } else {
        Ok(CheckoutResult::Switched {
            from,
            to: tag.to_string(),
        })
    }
}

/// 拉取最新 tag（git fetch --tags）
pub fn fetch_tags(repo: &Repository, remote_url: &str) -> Result<(), CoreError> {
    let mut remote = repo
        .remote("origin", remote_url)
        .map_err(|e| CoreError::GitError(e.to_string()))?;

    // fetch tags
    let mut fetch_opts = git2::FetchOptions::new();
    fetch_opts.download_tags(git2::AutotagOption::All);

    remote
        .fetch(&["refs/tags/*:refs/tags/*"], Some(&mut fetch_opts), None)
        .map_err(|e| CoreError::NetworkError(e.to_string()))?;

    Ok(())
}

/// 克隆仓库
///
/// 行为：
/// - 目录不存在 → 自动创建后 clone
/// - 目录存在但为空 → 直接 clone
/// - 目录已存在且非空且含 .git → 返回 `AlreadyExists`（已是仓库）
/// - 目录已存在且非空但不含 .git → 返回 `NotEmptyDir`（拒绝覆盖）
pub fn clone_repo(
    repo_path: &Path,
    url: &str,
) -> Result<(), CoreError> {
    // 1. 目录已存在
    if repo_path.exists() {
        let is_empty = !repo_path
            .read_dir()
            .map(|mut d| d.next().is_some())
            .unwrap_or(false);

        if !is_empty {
            // 目录非空：若已是 git 仓库，提示已存在；否则拒绝覆盖
            if repo_path.join(".git").exists() {
                return Err(CoreError::AlreadyExists(PathBuf::from(repo_path)));
            }
            return Err(CoreError::NotEmptyDir(PathBuf::from(repo_path)));
        }
    }

    // 2. 目录为空或不存在：尝试 clone
    //    libgit2 的 clone 在父目录不存在时会失败，所以预先创建父目录
    if let Some(parent) = repo_path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CoreError::GitError(format!("failed to create parent dir {:?}: {}", parent, e))
            })?;
        }
    }

    Repository::clone(url, repo_path).map_err(|e| CoreError::GitError(e.to_string()))?;
    tracing::info!(?repo_path, "comfyui repo cloned");
    Ok(())
}
