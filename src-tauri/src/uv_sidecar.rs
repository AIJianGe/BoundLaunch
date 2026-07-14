//! uv Sidecar 管理
//!
//! 目标：launcher 自带 uv 二进制，用户开箱即用，无需手动安装 uv。
//!
//! # 流程
//!
//! 1. **打包时**：`tauri.conf.json` 的 `bundle.resources` 包含 `resources/uv/*`
//!    - 文件名规范：`<name>-<target-triple>[.exe]`
//!    - 例如 Windows：`uv-x86_64-pc-windows-msvc.exe`
//!
//! 2. **启动时**：
//!    - 通过 `app.path().resource_dir()` 拿 Tauri 资源目录
//!    - 在资源目录找 `uv-<host-triple>[.exe]`
//!    - 复制到 `<app_data_dir>/uv/uv.exe`（首次或源文件变化时）
//!      - v1.8 / F38：跟随 portable 模式（dev → 项目根 data/，prod → exe 旁 data/）
//!    - Unix 上 `chmod +x`
//!
//! 3. **运行时**：`PythonEnvService` 用绝对路径调用 uv
//!
//! 4. **回退**：sidecar 文件不存在时退回到 PATH 查找（开发期）

use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager};

use crate::paths::env_paths;

/// 本 sidecar 版本（应与 `scripts/fetch-uv.ps1` 的 `$Version` 一致）
///
/// 变更时旧 release 目录的 uv.exe 会被覆盖（哈希校验）
pub const UV_SIDECAR_VERSION: &str = "0.4.18";

/// sidecar 二进制在 launcher 资源目录中的子目录
///
/// 注意：Tauri 2 的 `bundle.resources = ["resources/uv/*"]` 会把
/// `src-tauri/resources/uv/` 下的文件复制到 `target/<profile>/resources/uv/`。
/// 而 `app.path().resource_dir()` 返回 `target/<profile>/`（不含 `resources/`）。
/// 因此这里必须是 `"resources/uv"`，不能只是 `"uv"`。
const RESOURCE_SUBDIR: &str = "resources/uv";

/// sidecar 二进制在 launcher 资源目录中的文件名（含 target-triple 后缀）
///
/// Tauri 2 的资源文件保持原文件名；我们在解析时自己加 triple 后缀
fn bundled_binary_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        // 资源目录中应是 uv-x86_64-pc-windows-msvc.exe
        // 但我们用通配：uv-*.exe
        "uv.exe"
    }
    #[cfg(not(target_os = "windows"))]
    {
        "uv"
    }
}

/// launcher 用户数据目录中 sidecar 的存放位置
///
/// v0.0.2.1：固定在 `<env_root>/data/uv/uv.exe`（env_paths 唯一来源）
pub fn deployed_uv_path() -> PathBuf {
    env_paths::resolve()
        .map(|p| p.uv_binary_path.clone())
        .unwrap_or_else(|_| {
            // 解析失败兜底：用 find_exe_dir
            env_paths::find_exe_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("data")
                .join("uv")
                .join(if cfg!(windows) { "uv.exe" } else { "uv" })
        })
}

/// 当前 target 的 host triple（资源文件名后缀）
pub fn host_triple() -> &'static str {
    // Tauri 2 不直接暴露 host triple，构造它：
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    { "x86_64-pc-windows-msvc" }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    { "aarch64-pc-windows-msvc" }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    { "x86_64-apple-darwin" }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    { "aarch64-apple-darwin" }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    { "x86_64-unknown-linux-gnu" }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    { "aarch64-unknown-linux-gnu" }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
    )))]
    { compile_error!("unsupported host triple") }
}

/// 当前 target 期望的 bundled 二进制名（含 triple 后缀）
fn expected_bundled_filename() -> String {
    let triple = host_triple();
    let ext = if cfg!(windows) { ".exe" } else { "" };
    format!("uv-{}{}", triple, ext)
}

/// 在资源目录中找 bundled uv 二进制
///
/// 优先匹配 `uv-{host-triple}[.exe]`，找不到则退到 `uv.exe` / `uv`
fn locate_bundled_uv(resource_dir: &Path) -> Option<PathBuf> {
    let uv_dir = resource_dir.join(RESOURCE_SUBDIR);
    tracing::info!(
        ?resource_dir,
        ?uv_dir,
        subdir = RESOURCE_SUBDIR,
        "locate_bundled_uv: searching for uv binary"
    );
    if !uv_dir.exists() {
        tracing::warn!(
            ?uv_dir,
            "locate_bundled_uv: uv subdir does not exist"
        );
        // 列出 resource_dir 下的内容，帮助诊断
        if let Ok(entries) = std::fs::read_dir(resource_dir) {
            let listing: Vec<String> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect();
            tracing::info!(
                ?listing,
                "locate_bundled_uv: resource_dir contents"
            );
        }
        return None;
    }

    // 1) 严格匹配 host triple（生产 / 显式多平台打包场景）
    let expected = uv_dir.join(expected_bundled_filename());
    tracing::info!(?expected, "locate_bundled_uv: trying expected filename");
    if expected.exists() {
        tracing::info!("locate_bundled_uv: found via host-triple match");
        return Some(expected);
    }

    // 2) 退到 basename 匹配（开发场景，`resources/uv/uv.exe` 这样的简化命名）
    let fallback = uv_dir.join(bundled_binary_name());
    tracing::info!(?fallback, "locate_bundled_uv: trying fallback filename");
    if fallback.exists() {
        tracing::info!("locate_bundled_uv: found via fallback basename match");
        return Some(fallback);
    }

    // 列出 uv_dir 下的内容，帮助诊断
    if let Ok(entries) = std::fs::read_dir(&uv_dir) {
        let listing: Vec<String> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        tracing::warn!(
            ?listing,
            "locate_bundled_uv: uv_dir exists but no uv binary found, contents:"
        );
    }

    None
}

/// 确保 sidecar uv 已部署到用户数据目录
///
/// - bundled 二进制存在 + 部署路径不存在或哈希不同 → 复制 + chmod
/// - bundled 不存在 + 部署路径已存在 → 复用（升级前编译的 release）
/// - 都存在且一致 → 跳过
///
/// 返回最终应该使用的 uv 二进制路径（部署路径或 None）
pub async fn ensure_released(app: &AppHandle) -> Option<PathBuf> {
    // 1. 拿资源目录
    let resource_dir = match app.path().resource_dir() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "failed to get resource_dir, uv sidecar disabled");
            return None;
        }
    };

    // 2. 找 bundled uv
    let bundled = match locate_bundled_uv(&resource_dir) {
        Some(p) => p,
        None => {
            tracing::info!(
                ?resource_dir,
                "no bundled uv found in resources; uv sidecar disabled (fallback to PATH)"
            );
            return None;
        }
    };
    tracing::info!(?bundled, "found bundled uv sidecar");

    // 3. 准备部署路径
    let deployed = deployed_uv_path();
    if let Some(parent) = deployed.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            tracing::error!(?parent, error = %e, "failed to create uv deploy dir");
            return None;
        }
    }

    // 4. 比对内容决定是否需要重新部署
    let need_copy = if !deployed.exists() {
        tracing::info!(?deployed, "uv sidecar not yet deployed");
        true
    } else {
        match (tokio::fs::read(&bundled).await, tokio::fs::read(&deployed).await) {
            (Ok(b), Ok(d)) if b == d => {
                tracing::debug!("uv sidecar already up-to-date");
                false
            }
            _ => {
                tracing::info!("uv sidecar hash differs, redeploying");
                true
            }
        }
    };

    if need_copy {
        if let Err(e) = tokio::fs::copy(&bundled, &deployed).await {
            tracing::error!(?bundled, ?deployed, error = %e, "failed to copy uv sidecar");
            return None;
        }
        tracing::info!(?deployed, "uv sidecar deployed");
    }

    // 5. Unix: chmod +x
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = tokio::fs::metadata(&deployed).await {
            let mut perm = meta.permissions();
            if perm.mode() & 0o111 != 0o111 {
                perm.set_mode(0o755);
                let _ = tokio::fs::set_permissions(&deployed, perm).await;
                tracing::info!("uv sidecar chmod +x applied");
            }
        }
    }

    Some(deployed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_triple_matches_target() {
        let triple = host_triple();
        #[cfg(target_os = "windows")]
        assert!(triple.contains("windows"));
        #[cfg(target_os = "macos")]
        assert!(triple.contains("apple-darwin"));
        #[cfg(target_os = "linux")]
        assert!(triple.contains("unknown-linux-gnu"));
    }

    #[test]
    fn test_expected_filename_format() {
        let name = expected_bundled_filename();
        assert!(name.starts_with("uv-"));
        if cfg!(windows) {
            assert!(name.ends_with(".exe"));
        } else {
            assert!(!name.ends_with(".exe"));
        }
    }

    #[test]
    fn test_deployed_path_under_app_data() {
        let p = deployed_uv_path();
        // v0.0.2.1：固定在 <env_root>/data/uv/
        let env_paths_root = env_paths::resolve()
            .map(|p| p.app_data_dir.clone())
            .unwrap_or_else(|_| env_paths::find_exe_dir().unwrap_or_default().join("data"));
        assert!(p.starts_with(env_paths_root), "uv 应在 app_data_dir 下");
        assert!(p.ends_with("uv") || p.ends_with("uv.exe"));
    }
}
