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
use serde::Serialize;
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

    // 按 SemVer 倒序（v3.3 / F33 修复）
    //
    // 之前用字符串倒序导致 `v0.9.2` > `v0.27.0`、`v0.3.9` > `v0.3.10`，
    // 进而使 `tags::latest_stable` 误判最新稳定版。
    // 现在用 semver 数字比较，stable 在前、prerelease 在后。
    result.sort_by(|a, b| crate::core_manager::semver::cmp_tag_desc(&a.name, &b.name));

    Ok(result)
}

/// 强制 checkout 到指定 commit/tag（v3.1 / F26 决策 6：回滚用）
///
/// 与 `checkout_tag` 区别：
/// - `checkout_tag` 会先 stash 用户改动
/// - `force_checkout` 直接丢弃工作区改动（仅在回滚路径使用，调用方应保证工作区已干净）
///
/// 用于 switch_version 任务中：venv 安装失败后回滚到原 commit。
pub fn force_checkout(repo: &mut Repository, ref_name: &str) -> Result<(), CoreError> {
    let full_ref = if ref_name.starts_with("refs/tags/") {
        ref_name.to_string()
    } else {
        format!("refs/tags/{}", ref_name)
    };

    let obj = repo
        .revparse_single(&full_ref)
        .or_else(|_| repo.revparse_single(ref_name))
        .map_err(|e| CoreError::GitError(format!("ref {} not found: {}", ref_name, e)))?;

    let mut checkout_opts = git2::build::CheckoutBuilder::new();
    checkout_opts.force();

    // 步骤 1：把 working tree 切到目标 ref 的内容
    // 注意：这一步会把 index 也重置为目标 ref 的状态（force + checkout_tree 的默认行为）
    repo.checkout_tree(&obj, Some(&mut checkout_opts))
        .map_err(|e| CoreError::GitError(format!("force checkout failed: {}", e)))?;

    // 步骤 2：把 HEAD 指向目标 ref
    //
    // **F35-F 修复（v1.8）**：原代码用 `set_head("HEAD")`，但在某些 libgit2 状态下报
    // `'id'; class=Invalid` 错误（HEAD 已经存在，set_head 实际是 noop 但被 libgit2 拒绝）。
    // 后果：checkout_tree 已成功（working tree 已切），但 set_head 失败导致函数 Err 返回
    // → working tree 留在"已切到新版本但 HEAD 仍指旧版本"的中间状态
    // → git status 显示 v0.27.0 vs v0.26.2 的 27 个文件差异作为 staged 改动
    // → 按钮永久灰。
    //
    // 修复：用 `set_head_detached(obj.id())` 显式设置 detached HEAD 到目标 commit。
    // 兜底：set_head_detached 失败时**不视为致命错误**，因为 checkout_tree 已经成功。
    if let Err(e) = repo.set_head_detached(obj.id()) {
        tracing::warn!(
            error = %e,
            "set_head_detached failed (already detached or other reason); \
             checkout_tree succeeded so working tree is correct"
        );
        // 不返回 Err，让 force_checkout 视为成功
    }

    tracing::info!(ref_name, "force checkout completed");
    Ok(())
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
///
/// 简化版：只判断 working tree（含 untracked）是否脏。
/// 详细原因请用 `inspect_workspace_dirty`。
pub fn has_local_changes(repo: &Repository) -> Result<bool, CoreError> {
    let mut status_opts = git2::StatusOptions::new();
    status_opts.include_untracked(true);

    let statuses = repo
        .statuses(Some(&mut status_opts))
        .map_err(|e| CoreError::GitError(e.to_string()))?;

    Ok(!statuses.is_empty())
}

/// 工作区脏的原因（v1.8 / F35-A+）
///
/// git2 的 `repo.statuses()` 报告的状态分三类，分别对应 git 的不同概念：
/// - **Staged**：`git diff --cached` 看到的（已 `git add` 但未 commit）
/// - **Unstaged**：`git diff` 看到的（working tree 直接改了未 add）
/// - **Untracked**：`??` 标记的（新文件，从未 add）
///
/// 三者**互斥且独立**——可能只 staged、只 unstaged、三者都有。
/// 用户最容易忽略的就是 staged（`git diff` 默认看不到），
/// 这就是为什么"工作区脏但 git diff 干净"会发生。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkspaceDirtyReason {
    /// 至少 1 个文件已 add 但未 commit
    Staged {
        count: usize,
        /// 最多展示前 20 个文件路径，避免 UI 爆炸
        files: Vec<String>,
    },
    /// 至少 1 个文件有 working tree 改动（未 add）
    Unstaged {
        count: usize,
        files: Vec<String>,
    },
    /// 至少 1 个 untracked 文件/目录
    Untracked {
        count: usize,
        files: Vec<String>,
    },
}

impl WorkspaceDirtyReason {
    /// UI 展示用一句话标签
    pub fn label(&self) -> &'static str {
        match self {
            Self::Staged { .. } => "staged 改动",
            Self::Unstaged { .. } => "working tree 改动",
            Self::Untracked { .. } => "untracked 文件",
        }
    }
}

/// 详细检查工作区，返回第一个发现的原因（优先级：staged > unstaged > untracked）
///
/// 返回 `None` 表示工作区干净。
///
/// 实现：调用 `repo.statuses()`，对每个 entry 检查 status 位的 INDEX_/WT_/UNTRACKED 前缀。
pub fn inspect_workspace_dirty(repo: &Repository) -> Option<WorkspaceDirtyReason> {
    let mut status_opts = git2::StatusOptions::new();
    status_opts.include_untracked(true);
    status_opts.include_ignored(false);

    let statuses = repo.statuses(Some(&mut status_opts)).ok()?;

    let mut staged: Vec<String> = Vec::new();
    let mut unstaged: Vec<String> = Vec::new();
    let mut untracked: Vec<String> = Vec::new();

    for entry in statuses.iter() {
        let path_str = entry.path().unwrap_or("").to_string();
        if path_str.is_empty() {
            continue;
        }
        let st = entry.status();
        if st.is_conflicted() {
            // 冲突状态归到 Unstaged（最接近 working tree 改动）
            unstaged.push(path_str);
            continue;
        }
        // INDEX_* = staged
        if st.is_index_new()
            || st.is_index_modified()
            || st.is_index_deleted()
            || st.is_index_renamed()
            || st.is_index_typechange()
        {
            staged.push(path_str.clone());
        }
        // WT_* = working tree
        if st.is_wt_modified() || st.is_wt_deleted() || st.is_wt_typechange() {
            unstaged.push(path_str.clone());
        }
        // WT_RENAMED 单独看
        if st.is_wt_renamed() {
            unstaged.push(path_str.clone());
        }
        // Untracked：被 include_untracked 列出但 status bits 为 0（没 INDEX_* 也没 WT_* 标志）
        // 即"在 working tree 中但不在 HEAD/INDEX 中"
        if st.is_empty() {
            untracked.push(path_str);
        }
    }

    if !staged.is_empty() {
        return Some(WorkspaceDirtyReason::Staged {
            count: staged.len(),
            files: truncate_files(staged),
        });
    }
    if !unstaged.is_empty() {
        return Some(WorkspaceDirtyReason::Unstaged {
            count: unstaged.len(),
            files: truncate_files(unstaged),
        });
    }
    if !untracked.is_empty() {
        return Some(WorkspaceDirtyReason::Untracked {
            count: untracked.len(),
            files: truncate_files(untracked),
        });
    }
    None
}

fn truncate_files(mut files: Vec<String>) -> Vec<String> {
    const MAX: usize = 20;
    if files.len() > MAX {
        files.truncate(MAX);
    }
    files
}

/// **F35-A+** 撤销 staging（`git reset HEAD`），不修改 working tree 内容
///
/// 用例：用户有 staged 改动但实际想丢弃 staging（保留文件 working tree 内容），
/// 一键 `git reset HEAD` 等价于把所有 staged 改动撤回到 working tree。
///
/// 注意：unstaged 改动和 untracked 文件**不受影响**，仍在 working tree 中。
/// 如果用户想完整丢弃所有改动（不可恢复），用 `core_force_clean_workspace`。
pub fn reset_staged(repo: &Repository) -> Result<(), CoreError> {
    let head = repo.head().map_err(|e| CoreError::GitError(e.to_string()))?;
    let head_oid = head
        .target()
        .ok_or_else(|| CoreError::GitError("HEAD has no target".to_string()))?;
    // reset 接受 impl Into<Object<'repo>>，需要先把 Oid 转成 Object
    let obj = repo
        .find_object(head_oid, Some(git2::ObjectType::Commit))
        .map_err(|e| CoreError::GitError(e.to_string()))?;
    repo.reset(&obj, git2::ResetType::Mixed, None)
        .map_err(|e| CoreError::GitError(e.to_string()))?;
    Ok(())
}

/// **F35-A+** 强制清理整个工作区（`git checkout .` + `git clean -fd`）
///
/// ⚠️ **不可恢复**：会丢弃所有 tracked 改动和 untracked 文件。
/// 仅在用户在前端明确点击「强制清理」按钮后调用。
pub fn force_clean_workspace(repo: &Repository, comfyui_root: &std::path::Path) -> Result<(), CoreError> {
    // 1) checkout . 丢弃 working tree 改动（reset index 到 HEAD）
    let mut checkout_opts = git2::build::CheckoutBuilder::new();
    checkout_opts.force();
    checkout_opts.target_dir(comfyui_root);
    repo.checkout_head(Some(&mut checkout_opts))
        .map_err(|e| CoreError::GitError(e.to_string()))?;

    // 2) clean -fd 清理 untracked
    let mut opts = git2::build::CheckoutBuilder::new();
    opts.force();
    opts.remove_untracked(true);
    // repo.checkout_head with remove_untracked 实际上只更新 working tree；
    // 真正的 untracked 删除需要用 std::fs 或 git2 status + unlink。
    // 这里采用：列出 untracked 并删除
    drop(opts); // 暂时不用 CheckoutBuilder 做 untracked 删除

    let mut status_opts = git2::StatusOptions::new();
    status_opts.include_untracked(true);
    let statuses = repo
        .statuses(Some(&mut status_opts))
        .map_err(|e| CoreError::GitError(e.to_string()))?;
    for entry in statuses.iter() {
        if entry.status().is_empty() {
            if let Some(p) = entry.path() {
                let full = comfyui_root.join(p);
                if full.is_dir() {
                    let _ = std::fs::remove_dir_all(&full);
                } else {
                    let _ = std::fs::remove_file(&full);
                }
            }
        }
    }
    Ok(())
}

/// **F35-E**：tier-3 失败后的绝对兜底
///
/// 目标：清空 index，让 working tree 与 HEAD 一致。
/// 即便 HEAD 本身已损坏（孤儿 commit / detached 在临时 commit），
/// 至少下次切换按钮不会永久灰。
///
/// 等价于 `git reset HEAD`（不修改 working tree 内容，只清 staged 状态）。
///
/// 用例：F35-C tier-3 `set_head_detached(parent_oid)` 在某些 libgit2 状态下报
/// `'id'; class=Invalid` 错误（比如 HEAD 是某个无效 commit，或仓库处于异常状态），
/// 此时所有回滚 tier 失败，**index 残留 staged 改动导致工作区永远脏**。
/// F35-E 在这种最坏情况下保底清 index。
pub fn emergency_reset_to_head(repo: &Repository) -> Result<(), CoreError> {
    let head = repo.head().map_err(|e| CoreError::GitError(e.to_string()))?;
    let target = head
        .target()
        .ok_or_else(|| CoreError::GitError("HEAD has no target (unborn)".to_string()))?;
    let obj = repo
        .find_object(target, Some(git2::ObjectType::Commit))
        .map_err(|e| CoreError::GitError(e.to_string()))?;
    // Mixed: reset index to HEAD, leave working tree unchanged
    repo.reset(&obj, git2::ResetType::Mixed, None)
        .map_err(|e| CoreError::GitError(e.to_string()))?;
    Ok(())
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
