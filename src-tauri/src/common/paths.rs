//! 路径工具
//!
//! 集中管理所有应用路径，避免散落各处
//! 详见 `PR/03-模块设计/01-Config.md` (paths 配置)
//!
//! # 数据目录策略（v1.8 / F38 Portable 模式）
//!
//! 同一台电脑可以装多份 launcher 互不干扰。`app_data_dir()` 按以下优先级解析：
//!
//! 1. **环境变量 `BOUND_LAUNCH_DATA_DIR`**：完全手动指定（最高优先级）
//! 2. **Portable 模式**：`<exe_dir>/data/`（生产模式） 或 `<project_root>/data/`（dev 模式）
//!    - dev 模式识别：`cfg!(debug_assertions)` 编译期决定
//!    - 项目根：编译时通过 `CARGO_MANIFEST_DIR` 解析（= `src-tauri/` 的父目录）
//! 3. **Legacy fallback**：`<%APPDATA%>/boundlaunch/`（向后兼容，1.0 前老用户）

use std::path::{Path, PathBuf};

/// 环境变量名：完全手动指定数据目录
pub const ENV_DATA_DIR: &str = "BOUND_LAUNCH_DATA_DIR";

/// 数据目录的子目录名（exe 旁 / 项目根旁）
pub const DATA_SUBDIR: &str = "data";

/// Portable 模式下的 ComfyUI 仓库子目录名
pub const COMFYUI_SUBDIR: &str = "ComfyUI";

/// 应用数据目录（v1.8 / F38 重写：Portable 模式优先）
///
/// 解析顺序（详见模块文档）：
/// 1. `BOUND_LAUNCH_DATA_DIR` 环境变量
/// 2. Portable 模式：dev → `<project_root>/data/` / prod → `<exe_dir>/data/`
/// 3. Legacy fallback：`<%APPDATA%>/boundlaunch/`
///
/// 注意事项：
/// - 这是「数据目录」，里面放 config / logs / venv / transformers cache
/// - ComfyUI 仓库是 `portable_base_dir()/ComfyUI`，**不在 data/ 下**
/// - 老用户 `app_data_dir` 已存了 config.toml，本函数仍然能找到老位置
pub fn app_data_dir() -> PathBuf {
    // 1) 环境变量最高优先级
    if let Ok(p) = std::env::var(ENV_DATA_DIR) {
        let p = PathBuf::from(p);
        if !p.as_os_str().is_empty() {
            tracing::debug!(path = %p.display(), "app_data_dir: using BOUND_LAUNCH_DATA_DIR");
            return p;
        }
    }

    // 2) Portable 模式：dev → 项目根 / prod → exe 旁
    if let Some(base) = portable_base_dir() {
        let data = base.join(DATA_SUBDIR);
        tracing::debug!(path = %data.display(), "app_data_dir: using portable mode");
        return data;
    }

    // 3) Legacy fallback
    tracing::debug!(
        path = %dirs::data_dir()
            .map(|p| p.join("boundlaunch").display().to_string())
            .unwrap_or_default(),
        "app_data_dir: using legacy APPDATA fallback"
    );
    legacy_data_dir()
}

/// Portable 模式的基础目录（数据目录的父目录）
///
/// - **dev 模式** (`cfg!(debug_assertions)`)：项目根 = `CARGO_MANIFEST_DIR` 的父目录
///   - 即 `D:\AIWork\myComfyui\`（假设 manifest = `D:\AIWork\myComfyui\src-tauri`）
/// - **prod 模式**：可执行文件所在目录
///   - 即 `D:\myProInstallDir\boundLaunch\`
///
/// 返回 `None` 时调用方应降级到 legacy `app_data_dir`。
pub fn portable_base_dir() -> Option<PathBuf> {
    // dev 模式用项目根（编译期已知，避免 dev 时数据跑到 target/debug/ 下被 cargo clean 清掉）
    #[cfg(debug_assertions)]
    {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        if let Some(parent) = manifest_dir.parent() {
            return Some(parent.to_path_buf());
        }
    }

    // prod 模式用 exe 所在目录
    #[cfg(not(debug_assertions))]
    {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(parent) = exe.parent() {
                return Some(parent.to_path_buf());
            }
        }
    }

    // 兜底：尝试用 exe 目录（dev 模式 CARGO_MANIFEST_DIR 拿不到父目录时）
    #[allow(unreachable_code)]
    {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(parent) = exe.parent() {
                return Some(parent.to_path_buf());
            }
        }
        None
    }
}

/// Legacy 数据目录：`<%APPDATA%>/boundlaunch/`
///
/// 1.0 前老用户的位置。Portable 模式启用后，新用户不会到这里。
fn legacy_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
        .join("boundlaunch")
}

/// 应用数据目录在「数据迁移前的位置」（仅用于一次迁移检测）
///
/// Portable 模式启用后，启动器会探测 `legacy_data_dir()` 是否还有
/// 用户的旧 config / venv，如果有就提示迁移到新的 `app_data_dir()`。
pub fn legacy_data_dir_for_migration() -> PathBuf {
    legacy_data_dir()
}

/// launcher 工作目录（进程当前目录）
///
/// ComfyUI 根目录的默认值。当 config.toml 未配置 comfyui_root 时，
/// 使用此目录作为 ComfyUI 仓库的克隆位置。
///
/// 注意：v1.8 起更推荐使用 `portable_base_dir()` —— 后者会随 dev / prod 模式自动
/// 切换到项目根或 exe 旁。本函数保留用于「用户实际启动 launcher 时的 cwd」场景。
pub fn launcher_working_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// ComfyUI 根目录的默认值（v1.8 / F38 新增）
///
/// = `<portable_base_dir>/ComfyUI/`
///
/// 解析失败时降级到 `<launcher_working_dir>/ComfyUI`。
pub fn default_comfyui_root() -> PathBuf {
    if let Some(base) = portable_base_dir() {
        base.join(COMFYUI_SUBDIR)
    } else {
        launcher_working_dir().join(COMFYUI_SUBDIR)
    }
}

/// config.toml 路径
pub fn config_path() -> PathBuf {
    app_data_dir().join("config.toml")
}

/// launcher.sqlite 路径（LogStore）
pub fn log_db_path() -> PathBuf {
    app_data_dir().join("launcher.sqlite")
}

/// Cache 目录（与 data/ 平行的 sibling 目录）
///
/// v1.8 / F38：把 cache 从 data/ 拆出来，与 data/ 平行
/// - `data/`  : 持久状态（config / logs / sqlite / venv / pid）
/// - `cache/` : 可重新生成的缓存（transformers 版本索引等）
///
/// 路径：`<portable_base>/cache/`
/// - dev 模式：`<project_root>/cache/`
/// - prod 模式：`<exe_dir>/cache/`
/// - 兜底：legacy `<%APPDATA%>/boundlaunch/cache/`
pub fn cache_dir() -> PathBuf {
    portable_base_dir()
        .unwrap_or_else(legacy_data_dir)
        .join("cache")
}

/// transformers 版本缓存路径（v1.8 / F38：移到 cache/）
///
/// 之前在 `app_data_dir()/transformers_versions.json`，现在在
/// `cache_dir()/transformers_versions.json`。迁移时老位置的文件会被
/// 自动移动到新位置（见 `maybe_migrate_to_portable`）。
pub fn transformers_cache_path() -> PathBuf {
    cache_dir().join("transformers_versions.json")
}

/// uv sidecar 部署目录
pub fn uv_deploy_dir() -> PathBuf {
    app_data_dir().join("uv")
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

/// 探测并执行「Portable 数据迁移」（v1.8 / F38 新增）
///
/// 场景：老用户的 config / venv 还在 `<%APPDATA%>/boundlaunch/`，新版启用后
/// `app_data_dir()` 已经不再指向那里。需要：
/// 1. 检测老目录是否存在
/// 2. 检测新目录是否为空
/// 3. 把老内容复制到新位置
///
/// 返回：
/// - `Ok(MigrationOutcome::Migrated { from, to })`：迁移完成
/// - `Ok(MigrationOutcome::Noop)`：无需迁移（老目录空 / 新目录非空 / 已在新目录）
/// - `Err(e)`：迁移过程出错
pub async fn maybe_migrate_to_portable() -> anyhow::Result<MigrationOutcome> {
    use std::collections::HashSet;
    use tokio::fs;

    let new_dir = app_data_dir();
    let old_dir = legacy_data_dir_for_migration();

    // 1) 老目录不存在 → 无需迁移
    if !old_dir.exists() {
        return Ok(MigrationOutcome::Noop);
    }

    // 2) 新老路径相同（用户通过 env var 把新路径指到老位置）→ 无需迁移
    if old_dir == new_dir {
        return Ok(MigrationOutcome::Noop);
    }

    // 3) 老目录为空 → 无需迁移（仅有目录但无文件）
    let mut old_entries = fs::read_dir(&old_dir).await?;
    let mut has_real_file = false;
    while let Some(entry) = old_entries.next_entry().await? {
        let name = entry.file_name();
        let name_str = name.to_string_lossy().to_string();
        // 忽略迁移标记文件
        if name_str == MIGRATION_MARKER {
            continue;
        }
        has_real_file = true;
        break;
    }
    if !has_real_file {
        return Ok(MigrationOutcome::Noop);
    }

    // 4) 新目录已存在且非空 → 跳过迁移（避免覆盖）
    if new_dir.exists() {
        let mut new_entries = fs::read_dir(&new_dir).await?;
        while let Some(entry) = new_entries.next_entry().await? {
            let name = entry.file_name();
            let name_str = name.to_string_lossy().to_string();
            if name_str == MIGRATION_MARKER {
                continue;
            }
            // 新目录已有内容，不迁移（让用户手动合并）
            tracing::warn!(
                from = %old_dir.display(),
                to = %new_dir.display(),
                "portable migration skipped: target dir not empty"
            );
            return Ok(MigrationOutcome::Noop);
        }
    }

    // 5) 执行迁移：递归复制老目录 → 新目录
    tracing::info!(
        from = %old_dir.display(),
        to = %new_dir.display(),
        "portable migration: copying legacy data to new location"
    );
    ensure_dir(&new_dir).await?;
    copy_dir_recursive(&old_dir, &new_dir, &HashSet::new()).await?;

    // 6) v1.8 / F38：迁移后把老位置的 cache 文件搬到新 cache/ 目录
    //    之前 `transformers_versions.json` 在 data/ 下，现在 cache/ 与 data/ 分离
    //    这是一次性搬迁，搬完删老文件
    if let Err(e) = relocate_legacy_cache_file(&new_dir).await {
        tracing::warn!(error = %e, "F38: cache file relocation failed (non-fatal)");
    }

    // 7) 在老目录留标记文件，避免下次启动再迁移
    let marker = old_dir.join(MIGRATION_MARKER);
    let _ = fs::write(
        &marker,
        format!(
            "migrated to {} at {}",
            new_dir.display(),
            chrono::Utc::now().to_rfc3339()
        ),
    )
    .await;

    Ok(MigrationOutcome::Migrated {
        from: old_dir,
        to: new_dir,
    })
}

/// 搬迁老位置 `<new_dir>/transformers_versions.json` → `<new_dir>/cache/transformers_versions.json`
///
/// 只在 `maybe_migrate_to_portable` 复制完数据后调用一次。
/// - 旧文件存在 + 新位置不存在 → 移动（保持文件 mtime / inode）
/// - 旧文件不存在 → 跳过
/// - 新位置已存在 → 跳过（不覆盖）
async fn relocate_legacy_cache_file(new_dir: &Path) -> anyhow::Result<()> {
    use tokio::fs;

    let old_path = new_dir.join("transformers_versions.json");
    let new_cache_dir = new_dir.join("cache");
    let new_path = new_cache_dir.join("transformers_versions.json");

    if !old_path.exists() {
        return Ok(()); // 没有老文件，无需搬迁
    }
    if new_path.exists() {
        tracing::debug!("cache file already at new location, skip relocation");
        return Ok(());
    }

    fs::create_dir_all(&new_cache_dir).await?;
    fs::rename(&old_path, &new_path).await?;
    tracing::info!(
        from = %old_path.display(),
        to = %new_path.display(),
        "F38: relocated legacy cache file"
    );
    Ok(())
}

/// 迁移结果
#[derive(Debug, Clone)]
pub enum MigrationOutcome {
    /// 已迁移
    Migrated { from: PathBuf, to: PathBuf },
    /// 无需迁移
    Noop,
}

const MIGRATION_MARKER: &str = ".migrated-to-portable";

/// 递归复制目录（async）
async fn copy_dir_recursive(
    src: &Path,
    dst: &Path,
    _seen: &std::collections::HashSet<PathBuf>,
) -> anyhow::Result<()> {
    use tokio::fs;

    if !dst.exists() {
        fs::create_dir_all(dst).await?;
    }

    let mut entries = fs::read_dir(src).await?;
    while let Some(entry) = entries.next_entry().await? {
        let file_type = entry.file_type().await?;
        let dest_path = dst.join(entry.file_name());

        // 跳过迁移标记
        if entry.file_name().to_string_lossy() == MIGRATION_MARKER {
            continue;
        }

        if file_type.is_dir() {
            Box::pin(copy_dir_recursive(&entry.path(), &dest_path, _seen)).await?;
        } else if file_type.is_symlink() {
            // 符号链接：尝试复制链接目标（保持行为一致）
            let target = fs::read_link(&entry.path()).await?;
            if target.is_dir() {
                Box::pin(copy_dir_recursive(&target, &dest_path, _seen)).await?;
            } else {
                fs::copy(&target, &dest_path).await?;
            }
        } else {
            fs::copy(&entry.path(), &dest_path).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_data_subpaths() {
        // 各路径都在 app_data_dir 下
        let data = app_data_dir();
        assert!(config_path().starts_with(&data));
        assert!(log_db_path().starts_with(&data));
        assert!(pid_file_path().starts_with(&data));
        assert!(transformers_cache_path().starts_with(&data));
        assert!(uv_deploy_dir().starts_with(&data));
    }

    #[test]
    fn test_dirty_marker_under_comfyui_root() {
        let root = Path::new("/tmp/comfyui");
        let marker = dirty_marker_path(root);
        assert_eq!(marker, Path::new("/tmp/comfyui/.launcher-dirty"));
    }

    #[test]
    fn test_default_comfyui_root_uses_portable_base() {
        // 无论 dev 还是 prod，default_comfyui_root 都会落在 portable_base_dir 下
        if let Some(base) = portable_base_dir() {
            assert_eq!(default_comfyui_root(), base.join("ComfyUI"));
        }
    }
}
