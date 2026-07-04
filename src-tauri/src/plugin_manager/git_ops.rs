//! git2 操作封装（Adapter 模式）
//!
//! 设计模式：Adapter - 把 libgit2 C 库同步调用封装为 Rust 友好接口
//!
//! 关键约束（详见 `PR/02-技术架构.md §6 线程模型`）：
//! - git2 是同步 C 库，必须用 `tokio::task::spawn_blocking` 包裹移出 reactor
//! - 长任务（clone / pull）需要进度回调

use std::path::Path;

use git2::{build::RepoBuilder, FetchOptions, Repository, ResetType};

use super::models::PluginError;

/// 克隆插件仓库到指定目录
///
/// 失败时返回 `CloneFailed`，调用方负责清理半成品目录。
///
/// 注意：本函数不提供 clone 进度回调（git2 progress callback 与 spawn_blocking
/// 闭包 Send/Sync 约束复杂）。调用方应在调用前 emit `PluginProgress::Cloning { percent: 0 }`，
/// 调用完成后 emit `PluginProgress::Done` 或 `PluginProgress::Failed`。
pub fn clone_plugin_repo(url: &str, target_dir: &Path) -> Result<Repository, PluginError> {
    let mut builder = RepoBuilder::new();
    builder.fetch_options({
        let mut fo = FetchOptions::new();
        fo.download_tags(git2::AutotagOption::All);
        fo
    });

    match builder.clone(url, target_dir) {
        Ok(repo) => Ok(repo),
        Err(e) => Err(PluginError::CloneFailed {
            stderr: e.message().to_string(),
        }),
    }
}

/// 获取插件仓库的当前 commit hash
pub fn current_commit(repo: &Repository) -> Result<String, PluginError> {
    let head = repo.head()?;
    let commit = head.peel_to_commit()?;
    Ok(commit.id().to_string())
}

/// 获取插件仓库的当前分支名（detached HEAD 时返回 None）
pub fn current_branch(repo: &Repository) -> Result<Option<String>, PluginError> {
    match repo.head() {
        Ok(head) => {
            if head.is_branch() {
                let name = head.shorthand().map(|s| s.to_string());
                Ok(name)
            } else {
                Ok(None) // detached HEAD
            }
        }
        Err(_) => Ok(None),
    }
}

/// 检查仓库是否有本地未提交改动
pub fn has_local_changes(repo: &Repository) -> Result<bool, PluginError> {
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true);
    opts.include_ignored(false);
    let statuses = repo.statuses(Some(&mut opts))?;
    Ok(!statuses.is_empty())
}

/// 拉取远程更新（git pull origin <branch>）
///
/// 返回 (old_commit, new_commit)。若已是最新，两者相同。
pub fn pull_repo(repo: &Repository) -> Result<(String, String), PluginError> {
    let old_commit = current_commit(repo)?;

    // 找到当前分支的 upstream
    let head = repo.head()?;
    let branch_name = head.shorthand().ok_or_else(|| {
        PluginError::PullFailed {
            stderr: "detached HEAD, cannot pull".into(),
        }
    })?;

    let mut remote = repo.find_remote("origin").or_else(|_| {
        // fallback: 找第一个 remote
        let remotes = repo.remotes()?;
        if remotes.is_empty() {
            return Err(git2::Error::from_str("no remotes configured"));
        }
        let first = remotes.get(0).unwrap();
        repo.find_remote(first)
    })?;

    // fetch
    let mut fo = FetchOptions::new();
    fo.download_tags(git2::AutotagOption::All);
    remote.fetch(&[branch_name], Some(&mut fo), None).map_err(|e| {
        PluginError::PullFailed {
            stderr: e.message().to_string(),
        }
    })?;

    // 找到 fetch 后的远程分支 ref
    let remote_ref = format!("refs/remotes/origin/{}", branch_name);
    let fetch_head = repo.find_reference(&remote_ref)?;
    let fetch_commit = fetch_head.peel_to_commit()?;

    // merge: fast-forward
    let head_commit = head.peel_to_commit()?;
    if fetch_commit.id() == head_commit.id() {
        return Ok((old_commit.clone(), old_commit));
    }

    // 检查是否可以 fast-forward
    let ahead = repo.graph_ahead_behind(fetch_commit.id(), head_commit.id())?;
    let behind = repo.graph_ahead_behind(head_commit.id(), fetch_commit.id())?;
    let (ahead, behind) = (ahead.0, behind.0);

    if behind == 0 && ahead > 0 {
        // 可 fast-forward
        repo.reset(fetch_commit.as_object(), ResetType::Hard, None)?;
        let new_commit = current_commit(repo)?;
        return Ok((old_commit, new_commit));
    }

    // 有冲突，返回错误（要求用户手动处理）
    Err(PluginError::PullFailed {
        stderr: format!(
            "本地与远程有分叉（ahead={}, behind={}），需手动处理",
            ahead, behind
        ),
    })
}

/// 检查远程是否有更新（仅 fetch + 比较 commit，不修改本地）
pub fn check_remote_has_update(repo: &Repository) -> Result<bool, PluginError> {
    let head = repo.head()?;
    let branch_name = head.shorthand().ok_or_else(|| {
        PluginError::GitError(git2::Error::from_str("detached HEAD, cannot check"))
    })?;

    let mut remote = repo.find_remote("origin").or_else(|_| {
        let remotes = repo.remotes()?;
        if remotes.is_empty() {
            return Err(git2::Error::from_str("no remotes configured"));
        }
        let first = remotes.get(0).unwrap();
        repo.find_remote(first)
    })?;

    let mut fo = FetchOptions::new();
    fo.download_tags(git2::AutotagOption::None);
    remote.fetch(&[branch_name], Some(&mut fo), None)?;

    let remote_ref = format!("refs/remotes/origin/{}", branch_name);
    let fetch_commit = repo.find_reference(&remote_ref)?.peel_to_commit()?;
    let head_commit = head.peel_to_commit()?;
    Ok(fetch_commit.id() != head_commit.id())
}

/// 从仓库配置读取 remote URL
pub fn remote_url(repo: &Repository) -> Option<String> {
    let remote = repo.find_remote("origin").ok()?;
    remote.url().map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    /// 创建一个本地 git 仓库（用于测试）
    fn make_test_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();
        // 配置 user
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test").unwrap();
        config.set_str("user.email", "test@test.com").unwrap();
        // 创建一个 commit
        fs::write(tmp.path().join("README.md"), "# test\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("README.md")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "initial commit",
            &tree,
            &[],
        )
        .unwrap();
        tmp
    }

    #[test]
    fn test_current_commit_returns_hash() {
        let tmp = make_test_repo();
        let repo = Repository::open(tmp.path()).unwrap();
        let commit = current_commit(&repo).unwrap();
        assert!(!commit.is_empty());
        assert_eq!(commit.len(), 40); // SHA-1 hash 长度
    }

    #[test]
    fn test_current_branch_returns_main_or_master() {
        let tmp = make_test_repo();
        let repo = Repository::open(tmp.path()).unwrap();
        let branch = current_branch(&repo).unwrap();
        assert!(branch.is_some());
        let name = branch.unwrap();
        assert!(name == "main" || name == "master");
    }

    #[test]
    fn test_has_local_changes_clean() {
        let tmp = make_test_repo();
        let repo = Repository::open(tmp.path()).unwrap();
        assert!(!has_local_changes(&repo).unwrap());
    }

    #[test]
    fn test_has_local_changes_dirty() {
        let tmp = make_test_repo();
        let repo = Repository::open(tmp.path()).unwrap();
        fs::write(tmp.path().join("new.txt"), "new content").unwrap();
        assert!(has_local_changes(&repo).unwrap());
    }

    #[test]
    fn test_clone_plugin_repo_from_local() {
        let src = make_test_repo();
        let dst = tempfile::tempdir().unwrap();

        // 用 file:// 协议克隆本地仓库（仅测试用，生产环境校验 https://）
        // Windows 路径需转换为正斜杠并使用三斜杠 file:///<path> 格式，
        // 否则 file://C:\... 会被解析为主机名 "C:" 导致 "卷标语法不正确"
        let path_str = src.path().to_string_lossy().replace('\\', "/");
        let url = format!("file:///{}", path_str);
        let repo = clone_plugin_repo(&url, dst.path()).unwrap();
        assert!(dst.path().join(".git").exists());
        let commit = current_commit(&repo).unwrap();
        assert_eq!(commit.len(), 40);
    }

    #[test]
    fn test_remote_url_returns_none_for_no_remote() {
        let tmp = make_test_repo();
        let repo = Repository::open(tmp.path()).unwrap();
        assert!(remote_url(&repo).is_none());
    }

    #[test]
    fn test_check_remote_has_update_no_remote() {
        let tmp = make_test_repo();
        let repo = Repository::open(tmp.path()).unwrap();
        let result = check_remote_has_update(&repo);
        assert!(result.is_err());
    }

    /// 用 `git` CLI 添加一个 remote（测试用，避免修改 git2 复杂 API）
    fn add_remote(repo_path: &Path, name: &str, url: &str) {
        let _ = Command::new("git")
            .args(["remote", "add", name, url])
            .current_dir(repo_path)
            .output();
    }
}
