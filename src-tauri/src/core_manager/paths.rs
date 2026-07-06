//! 软链接管理（v3.1 / F26）
//!
//! 用于 ComfyUI 仓库内 `models/` 等数据目录的跨版本共享。
//!
//! ## 设计模式
//! - **Adapter**：跨平台差异（Windows junction / Unix symlink）封装为统一接口
//! - **Strategy**：根据目标路径状态选择不同迁移策略
//!
//! ## 关键约束
//! - Windows junction 不需要管理员权限（vs symlink 需要开发者模式或管理员）
//! - junction 只支持绝对路径 + 本地卷
//! - 切换 ComfyUI 版本前必须先解除链接，避免 git checkout 冲突
//! - 切换后重新建立链接，确保用户模型数据不丢失
//!
//! 详见 `PR/03-模块设计/03-CoreManager.md §6 软链接管理`（F26 新增）

use std::path::{Path, PathBuf};

use crate::error::CoreError;

/// 软链接类型（用于日志和事件）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// Windows junction（NTFS 重解析点）
    Junction,
    /// Unix symlink
    Symlink,
}

impl LinkKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Junction => "junction",
            Self::Symlink => "symlink",
        }
    }
}

/// 检测路径是否为软链接（junction / symlink）
pub fn is_link(path: &Path) -> bool {
    #[cfg(windows)]
    {
        junction::exists(path).unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        std::fs::symlink_metadata(path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    }
}

/// 创建软链接：link_path → target_path
///
/// - Windows：使用 junction（不需要管理员权限）
/// - Unix：使用 symlink
///
/// # 错误
/// - `link_path` 已存在 → `CoreError::GitError`（调用方应先 `remove_link`）
/// - `target_path` 不存在 → 会自动创建目录
/// - 平台 API 失败 → `CoreError::GitError`
pub fn create_link(link_path: &Path, target_path: &Path) -> Result<LinkKind, CoreError> {
    // 确保父目录存在
    if let Some(parent) = link_path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CoreError::GitError(format!(
                    "create parent dir {:?} failed: {}",
                    parent, e
                ))
            })?;
        }
    }

    // 确保 target_path 存在（用户首次配置 models_path 时可能还没创建）
    if !target_path.exists() {
        std::fs::create_dir_all(target_path).map_err(|e| {
            CoreError::GitError(format!(
                "create target dir {:?} failed: {}",
                target_path, e
            ))
        })?;
    }

    #[cfg(windows)]
    {
        junction::create(target_path, link_path).map_err(|e| {
            CoreError::GitError(format!(
                "create junction {:?} -> {:?} failed: {}",
                link_path, target_path, e
            ))
        })?;
        tracing::info!(
            ?link_path,
            ?target_path,
            "junction created"
        );
        Ok(LinkKind::Junction)
    }

    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(target_path, link_path).map_err(|e| {
            CoreError::GitError(format!(
                "create symlink {:?} -> {:?} failed: {}",
                link_path, target_path, e
            ))
        })?;
        tracing::info!(
            ?link_path,
            ?target_path,
            "symlink created"
        );
        Ok(LinkKind::Symlink)
    }
}

/// 删除软链接（不删除 target 内容）
///
/// 幂等：link_path 不存在或不是链接时返回 Ok
pub fn remove_link(link_path: &Path) -> Result<(), CoreError> {
    if !link_path.exists() && !is_link(link_path) {
        return Ok(());
    }

    if !is_link(link_path) {
        // 不是链接，可能是真实目录 - 拒绝删除以免数据丢失
        return Err(CoreError::GitError(format!(
            "path {:?} is not a link (refuse to remove real directory)",
            link_path
        )));
    }

    #[cfg(windows)]
    {
        // junction::delete 会删除重解析点但保留目标内容
        // 但如果 link_path 本身是 junction，删除它会同时删除"链接条目"而不影响目标
        junction::delete(link_path).map_err(|e| {
            CoreError::GitError(format!("remove junction {:?} failed: {}", link_path, e))
        })?;
        tracing::info!(?link_path, "junction removed");
    }

    #[cfg(not(windows))]
    {
        std::fs::remove_file(link_path).map_err(|e| {
            CoreError::GitError(format!("remove symlink {:?} failed: {}", link_path, e))
        })?;
        tracing::info!(?link_path, "symlink removed");
    }

    Ok(())
}

/// 获取链接指向的真实目标路径
pub fn read_link_target(link_path: &Path) -> Result<Option<PathBuf>, CoreError> {
    if !is_link(link_path) {
        return Ok(None);
    }

    #[cfg(windows)]
    {
        junction::get_target(link_path)
            .map(Some)
            .map_err(|e| CoreError::GitError(format!("read junction target failed: {}", e)))
    }

    #[cfg(not(windows))]
    {
        std::fs::read_link(link_path)
            .map(Some)
            .map_err(|e| CoreError::GitError(format!("read symlink target failed: {}", e)))
    }
}

/// 解析 models 路径：根据 Config.paths.models_path 决定最终路径
///
/// - `Some(custom)` → `custom`
/// - `None` → `<comfyui_root>/models`（ComfyUI 默认）
pub fn resolve_models_path(comfyui_root: &Path, models_path: Option<&Path>) -> PathBuf {
    match models_path {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => comfyui_root.join("models"),
    }
}

/// 确保 ComfyUI 仓库内的 `models/` 软链接到自定义路径
///
/// 在 ComfyUI 启动前调用。行为：
/// 1. 解析最终 models 路径（自定义 or `<comfyui_root>/models`）
/// 2. 若 `<comfyui_root>/models` 已是链接 → 校验 target 一致 → 跳过
/// 3. 若 `<comfyui_root>/models` 是真实目录 → 拒绝（用户应先迁移数据）→ 返回错误
/// 4. 若 `<comfyui_root>/models` 不存在 → 创建链接
///
/// 切换 ComfyUI 版本时，先 `remove_link`，git checkout 后再 `ensure_models_link`。
pub fn ensure_models_link(
    comfyui_root: &Path,
    custom_models_path: Option<&Path>,
) -> Result<Option<LinkKind>, CoreError> {
    let link_in_repo = comfyui_root.join("models");

    // 未配置自定义路径 - 不需要链接
    let custom = match custom_models_path {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => return Ok(None),
    };

    // 校验自定义路径与 ComfyUI 仓库路径不重叠
    let custom_abs = custom.canonicalize().unwrap_or_else(|_| custom.to_path_buf());
    let repo_abs = comfyui_root
        .canonicalize()
        .unwrap_or_else(|_| comfyui_root.to_path_buf());
    if custom_abs == repo_abs.join("models") {
        // 自定义路径就是 ComfyUI 默认路径，无需链接
        return Ok(None);
    }

    // 链接已存在且 target 一致 → 跳过
    if is_link(&link_in_repo) {
        let current_target = read_link_target(&link_in_repo)?;
        if let Some(t) = current_target {
            let t_abs = t.canonicalize().unwrap_or(t);
            if t_abs == custom_abs {
                tracing::debug!(
                    ?link_in_repo,
                    ?custom_abs,
                    "models link already points to custom path, skip"
                );
                return Ok(None);
            }
        }
        // target 不一致 - 先移除旧链接
        tracing::info!(
            ?link_in_repo,
            "removing existing link with different target"
        );
        remove_link(&link_in_repo)?;
    } else if link_in_repo.exists() {
        // 真实目录存在 - 拒绝覆盖（用户应先迁移数据）
        return Err(CoreError::GitError(format!(
            "{:?} is a real directory, not a link. \
            Please migrate your models to {:?} first, then remove the directory.",
            link_in_repo, custom_abs
        )));
    }

    // 创建新链接
    let kind = create_link(&link_in_repo, custom)?;
    Ok(Some(kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_models_path_default() {
        let root = Path::new("/tmp/comfyui");
        let resolved = resolve_models_path(root, None);
        assert_eq!(resolved, root.join("models"));
    }

    #[test]
    fn test_resolve_models_path_custom() {
        let root = Path::new("/tmp/comfyui");
        let custom = Path::new("/data/models");
        let resolved = resolve_models_path(root, Some(custom));
        assert_eq!(resolved, custom);
    }

    #[test]
    fn test_resolve_models_path_empty_string_uses_default() {
        let root = Path::new("/tmp/comfyui");
        let custom = Path::new("");
        let resolved = resolve_models_path(root, Some(custom));
        assert_eq!(resolved, root.join("models"));
    }

    #[tokio::test]
    async fn test_link_create_remove_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target_models");
        let link = tmp.path().join("link_to_models");

        // 创建链接
        let kind = create_link(&link, &target).expect("create link");
        assert!(is_link(&link));
        assert!(target.exists()); // target 应已被自动创建

        // 读取 target
        let t = read_link_target(&link).expect("read target");
        assert!(t.is_some());

        // 删除链接
        remove_link(&link).expect("remove link");
        assert!(!link.exists());
        assert!(target.exists()); // target 内容应保留

        let _ = kind; // 编译期使用 kind
    }

    #[test]
    fn test_remove_link_idempotent_for_nonexistent() {
        let tmp = tempfile::tempdir().unwrap();
        let nonexistent = tmp.path().join("does_not_exist");
        // 不存在的路径应返回 Ok（幂等）
        assert!(remove_link(&nonexistent).is_ok());
    }

    #[test]
    fn test_remove_link_refuses_real_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let real_dir = tmp.path().join("real_dir");
        std::fs::create_dir_all(&real_dir).unwrap();
        // 真实目录应返回错误
        let result = remove_link(&real_dir);
        assert!(result.is_err());
    }
}
