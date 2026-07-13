//! 更新应用：白名单拷贝 + 重启时 rename
//!
//! ## 关键设计：白名单替换
//!
//! 启动器更新**只能**触碰三类文件，其他文件（用户配置、ComfyUI、venv、模型、插件）
//! 一律保留。
//!
//! | 白名单 | 类别 | 替换策略 |
//! |---|---|---|
//! | `BoundLaunch.exe` | 程序本体 | staging → pending/BoundLaunch.exe.new |
//! | `BoundLaunch.dll` | 运行时 | staging → pending/BoundLaunch.dll.new |
//! | `resources/uv/*` | 内置 uv | staging → pending/resources/uv/ (直接覆盖) |
//!
//! 其他文件：忽略（zip 可能含 launcher-portable.dat、README、CHANGELOG 等，都不复制）
//!
//! ## 两个阶段
//!
//! - `apply_update(staging_dir)`：把 staging 白名单拷贝到 pending（生成 .new）
//! - `apply_pending_update()`：启动时调用，把 .new rename 成标准名
//!
//! ## 可测试性
//!
//! - `apply_update_to(staging, pending)` / `apply_pending_update_with(pending, env_root)`
//!   接受显式路径参数，**不依赖** `env_paths::resolve()`，方便单元测试
//! - `apply_update()` / `apply_pending_update()` 是便捷包装，从 `paths::pending_dir()` 取

use std::path::{Path, PathBuf};

use super::paths;
use crate::error::ProcessError;

/// 替换白名单
const REPLACE_EXE: &str = "BoundLaunch.exe";
const REPLACE_DLL: &str = "BoundLaunch.dll";
const REPLACE_UV_DIR: &str = "resources/uv";

/// 第一阶段（便捷包装）：用默认 `paths::pending_dir()` 应用更新
pub fn apply_update(staging_dir: &Path) -> Result<ApplyResult, ProcessError> {
    apply_update_to(staging_dir, &paths::pending_dir())
}

/// 第一阶段（可测试）：把 staging 内容按白名单挪到 `pending_dir`
///
/// 完成后 staging 目录被清空，pending 目录待用户重启后由 `apply_pending_update` 处理
///
/// **为什么拆出 `_to` 版本**：
/// - 默认 `apply_update()` 内部调 `env_paths::resolve()`，测试时难造 portable.dat
/// - `_to` 版本接受显式路径，单元测试可以传临时目录，零依赖
pub fn apply_update_to(
    staging_dir: &Path,
    pending_dir: &Path,
) -> Result<ApplyResult, ProcessError> {
    if !staging_dir.exists() {
        return Err(ProcessError::Other(format!(
            "staging 目录不存在: {}",
            staging_dir.display()
        )));
    }

    let pending = pending_dir;
    if pending.exists() {
        // 清理旧 pending（说明之前有未应用的更新，用户一直未重启）
        std::fs::remove_dir_all(pending)
            .map_err(|e| ProcessError::Other(format!("清理旧 pending 失败: {}", e)))?;
    }
    std::fs::create_dir_all(pending)
        .map_err(|e| ProcessError::Other(format!("创建 pending 失败: {}", e)))?;

    let mut result = ApplyResult::default();

    // 1) BoundLaunch.exe → pending/BoundLaunch.exe.new
    let exe_src = staging_dir.join(REPLACE_EXE);
    if exe_src.exists() {
        let exe_dst = pending.join(format!("{}.new", REPLACE_EXE));
        std::fs::copy(&exe_src, &exe_dst)
            .map_err(|e| ProcessError::Other(format!("拷贝 {} 失败: {}", REPLACE_EXE, e)))?;
        tracing::info!(?exe_dst, "已准备 exe 挂起更新");
        result.exe_pending = Some(exe_dst);
    } else {
        return Err(ProcessError::Other(format!(
            "更新包缺少必需文件: {}",
            REPLACE_EXE
        )));
    }

    // 2) BoundLaunch.dll → pending/BoundLaunch.dll.new
    let dll_src = staging_dir.join(REPLACE_DLL);
    if dll_src.exists() {
        let dll_dst = pending.join(format!("{}.new", REPLACE_DLL));
        std::fs::copy(&dll_src, &dll_dst)
            .map_err(|e| ProcessError::Other(format!("拷贝 {} 失败: {}", REPLACE_DLL, e)))?;
        tracing::info!(?dll_dst, "已准备 dll 挂起更新");
        result.dll_pending = Some(dll_dst);
    } else {
        // dll 缺失不报错（部分打包可能不包含 dll）
        tracing::warn!("更新包不含 {}（可忽略）", REPLACE_DLL);
    }

    // 3) resources/uv/* → pending/resources/uv/（合并覆盖）
    let uv_src = staging_dir.join(REPLACE_UV_DIR);
    if uv_src.exists() {
        let uv_dst = pending.join(REPLACE_UV_DIR);
        copy_dir_merge(&uv_src, &uv_dst)?;
        tracing::info!(?uv_dst, "已准备 uv 资源挂起更新");
        result.uv_pending = Some(uv_dst);
    } else {
        tracing::warn!("更新包不含 {}（可忽略）", REPLACE_UV_DIR);
    }

    // 4) 清理 staging
    let _ = std::fs::remove_dir_all(staging_dir);

    Ok(result)
}

/// 第二阶段（便捷包装）：用默认 `paths::pending_dir()` 和 `paths::env_root()` 应用 pending 更新
pub fn apply_pending_update() -> ApplyPendingResult {
    apply_pending_update_with(&paths::pending_dir(), &paths::env_root())
}

/// 第二阶段（可测试）：检测并应用 pending 更新
///
/// - 检测 `pending_dir` 目录是否存在
/// - 删除旧 exe/dll，把 .new rename 成标准名
/// - resources/uv/ 已直接合并到 pending/，再 merge 到 `env_root`
/// - 清空 pending
pub fn apply_pending_update_with(
    pending_dir: &Path,
    env_root: &Path,
) -> ApplyPendingResult {
    let mut result = ApplyPendingResult::default();

    if !pending_dir.exists() {
        return result;
    }

    // 1) BoundLaunch.exe
    let exe_new = pending_dir.join(format!("{}.new", REPLACE_EXE));
    let exe_target = env_root.join(REPLACE_EXE);
    if exe_new.exists() {
        if exe_target.exists() {
            if let Err(e) = std::fs::remove_file(&exe_target) {
                tracing::warn!(error = %e, "删除旧 exe 失败，跳过 exe 更新");
            }
        }
        match std::fs::rename(&exe_new, &exe_target) {
            Ok(()) => {
                tracing::info!(?exe_target, "exe 已更新");
                result.exe_updated = true;
            }
            Err(e) => {
                tracing::warn!(error = %e, "rename exe.new 失败");
            }
        }
    }

    // 2) BoundLaunch.dll
    let dll_new = pending_dir.join(format!("{}.new", REPLACE_DLL));
    let dll_target = env_root.join(REPLACE_DLL);
    if dll_new.exists() {
        if dll_target.exists() {
            let _ = std::fs::remove_file(&dll_target);
        }
        match std::fs::rename(&dll_new, &dll_target) {
            Ok(()) => {
                tracing::info!(?dll_target, "dll 已更新");
                result.dll_updated = true;
            }
            Err(e) => {
                tracing::warn!(error = %e, "rename dll.new 失败");
            }
        }
    }

    // 3) resources/uv/
    let uv_pending = pending_dir.join(REPLACE_UV_DIR);
    if uv_pending.exists() {
        let uv_target = env_root.join(REPLACE_UV_DIR);
        match copy_dir_merge(&uv_pending, &uv_target) {
            Ok(()) => {
                tracing::info!(?uv_target, "uv 资源已合并");
                result.uv_updated = true;
            }
            Err(e) => {
                tracing::warn!(error = %e, "uv 资源合并失败");
            }
        }
    }

    // 4) 清空 pending
    if let Err(e) = std::fs::remove_dir_all(pending_dir) {
        tracing::warn!(error = %e, "清理 pending 失败");
    }

    result
}

/// 合并复制：把 src 的所有内容拷贝到 dst（dst 不存在则创建，存在则覆盖合并）
fn copy_dir_merge(src: &Path, dst: &Path) -> Result<(), ProcessError> {
    if !dst.exists() {
        std::fs::create_dir_all(dst)
            .map_err(|e| ProcessError::Other(format!("创建目录失败: {}", e)))?;
    }

    for entry in std::fs::read_dir(src)
        .map_err(|e| ProcessError::Other(format!("读取目录失败: {}", e)))?
    {
        let entry = entry
            .map_err(|e| ProcessError::Other(format!("读取 entry 失败: {}", e)))?;
        let file_type = entry
            .file_type()
            .map_err(|e| ProcessError::Other(format!("读取 file_type 失败: {}", e)))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if file_type.is_dir() {
            copy_dir_merge(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            // 覆盖
            std::fs::copy(&src_path, &dst_path)
                .map_err(|e| ProcessError::Other(format!("拷贝文件失败: {}", e)))?;
        }
    }
    Ok(())
}

/// `apply_update` 的结果
#[derive(Debug, Default, serde::Serialize)]
pub struct ApplyResult {
    /// exe 挂起路径
    pub exe_pending: Option<PathBuf>,
    /// dll 挂起路径
    pub dll_pending: Option<PathBuf>,
    /// uv 资源挂起路径
    pub uv_pending: Option<PathBuf>,
}

/// `apply_pending_update` 的结果
#[derive(Debug, Default, serde::Serialize)]
pub struct ApplyPendingResult {
    pub exe_updated: bool,
    pub dll_updated: bool,
    pub uv_updated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("updater_apply_test_{}", name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_copy_dir_merge_new() {
        let src = temp_dir("merge_src");
        let dst = temp_dir("merge_dst");

        std::fs::write(src.join("a.txt"), "hello").unwrap();
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("sub/b.txt"), "world").unwrap();

        copy_dir_merge(&src, &dst).unwrap();

        assert!(dst.join("a.txt").exists());
        assert!(dst.join("sub/b.txt").exists());

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn test_copy_dir_merge_overwrite() {
        let src = temp_dir("merge_src2");
        let dst = temp_dir("merge_dst2");

        std::fs::write(src.join("a.txt"), "new").unwrap();
        std::fs::write(dst.join("a.txt"), "old").unwrap();
        std::fs::write(dst.join("keep.txt"), "keep").unwrap();

        copy_dir_merge(&src, &dst).unwrap();

        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "new");
        assert_eq!(
            std::fs::read_to_string(dst.join("keep.txt")).unwrap(),
            "keep"
        );

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    // ========================================================================
    // 端到端测试：apply_update_to + apply_pending_update_with
    // ========================================================================
    //
    // 模拟一次完整更新流程：
    // 1. 准备 staging（模拟下载 + 解压后的目录）
    // 2. 准备 env_root（模拟用户当前安装的绿色版）
    // 3. apply_update_to：白名单 → pending/BoundLaunch.exe.new 等
    // 4. apply_pending_update_with：模拟"用户重启" → .new rename 成标准名
    // 5. 验证 env_root 的 exe/dll/uv 已被更新到 staging 的版本
    // 6. 验证非白名单文件（用户数据）完全不动

    /// 模拟"解压后的 staging"——一个真实的 release zip 解压后应有的目录
    fn build_fake_staging(staging: &Path) {
        // 白名单：BoundLaunch.exe
        std::fs::write(staging.join("BoundLaunch.exe"), b"NEW_EXE_v2").unwrap();
        // 白名单：BoundLaunch.dll
        std::fs::write(staging.join("BoundLaunch.dll"), b"NEW_DLL_v2").unwrap();
        // 白名单：resources/uv/*
        let uv = staging.join("resources").join("uv");
        std::fs::create_dir_all(&uv).unwrap();
        std::fs::write(uv.join("uv.exe"), b"NEW_UV_v2").unwrap();
        // 非白名单（应被忽略，不进 pending）：launcher-portable.dat
        std::fs::write(
            staging.join("launcher-portable.dat"),
            b"portable_config_v2",
        )
        .unwrap();
        // 非白名单：README.md
        std::fs::write(staging.join("README.md"), b"new readme").unwrap();
        // 非白名单：.gitignore
        std::fs::write(staging.join(".gitignore"), b"v2").unwrap();
    }

    /// 模拟"用户当前安装的绿色版"
    fn build_fake_env_root(env_root: &Path) {
        // 已有 BoundLaunch.exe（应被替换为新版）
        std::fs::write(env_root.join("BoundLaunch.exe"), b"OLD_EXE_v1").unwrap();
        // 已有 BoundLaunch.dll
        std::fs::write(env_root.join("BoundLaunch.dll"), b"OLD_DLL_v1").unwrap();
        // 已有 resources/uv/uv.exe（应被合并覆盖）
        let uv = env_root.join("resources").join("uv");
        std::fs::create_dir_all(&uv).unwrap();
        std::fs::write(uv.join("uv.exe"), b"OLD_UV_v1").unwrap();
        // 用户数据：ComfyUI 核心（**绝对不能动**）
        std::fs::create_dir_all(env_root.join("ComfyUI")).unwrap();
        std::fs::write(
            env_root.join("ComfyUI").join("main.py"),
            b"print('comfyui core')",
        )
        .unwrap();
        // 用户数据：自定义配置（**绝对不能动**）
        std::fs::write(
            env_root.join("launcher-portable.dat"),
            b"portable_config_user_modified",
        )
        .unwrap();
        // 用户数据：venv（**绝对不能动**）
        std::fs::create_dir_all(env_root.join("data").join("venv")).unwrap();
        std::fs::write(
            env_root
                .join("data")
                .join("venv")
                .join("pyvenv.cfg"),
            b"home = /usr/bin/python3",
        )
        .unwrap();
        // 用户数据：模型（**绝对不能动**）
        std::fs::create_dir_all(env_root.join("ComfyUI").join("models")).unwrap();
        std::fs::write(
            env_root
                .join("ComfyUI")
                .join("models")
                .join("big_model.safetensors"),
            b"FAKE_MODEL_CONTENT_4GB",
        )
        .unwrap();
    }

    #[test]
    fn test_apply_update_to_end_to_end() {
        let staging = temp_dir("e2e_staging");
        let pending = temp_dir("e2e_pending");
        let env_root = temp_dir("e2e_env_root");

        build_fake_staging(&staging);
        build_fake_env_root(&env_root);

        // ===== 第一阶段：白名单 → pending =====
        let apply = apply_update_to(&staging, &pending).expect("apply_update_to failed");

        // staging 已被清空
        assert!(!staging.exists(), "staging 应该被清空");

        // pending 里有 .new
        assert!(apply.exe_pending.is_some());
        assert!(apply.dll_pending.is_some());
        assert!(apply.uv_pending.is_some());
        assert!(pending.join("BoundLaunch.exe.new").exists());
        assert!(pending.join("BoundLaunch.dll.new").exists());
        assert!(pending.join("resources/uv/uv.exe").exists());

        // pending 里**没有**非白名单文件
        assert!(!pending.join("launcher-portable.dat").exists());
        assert!(!pending.join("README.md").exists());
        assert!(!pending.join(".gitignore").exists());

        // ===== 第二阶段：模拟"用户重启" =====
        let pending_apply = apply_pending_update_with(&pending, &env_root);

        assert!(pending_apply.exe_updated);
        assert!(pending_apply.dll_updated);
        assert!(pending_apply.uv_updated);

        // 验证 env_root 的白名单文件被更新
        assert_eq!(
            std::fs::read_to_string(env_root.join("BoundLaunch.exe")).unwrap(),
            "NEW_EXE_v2"
        );
        assert_eq!(
            std::fs::read_to_string(env_root.join("BoundLaunch.dll")).unwrap(),
            "NEW_DLL_v2"
        );
        assert_eq!(
            std::fs::read_to_string(env_root.join("resources/uv/uv.exe")).unwrap(),
            "NEW_UV_v2"
        );

        // 验证用户数据完全未动
        assert_eq!(
            std::fs::read_to_string(
                env_root.join("ComfyUI").join("main.py")
            )
            .unwrap(),
            "print('comfyui core')"
        );
        assert_eq!(
            std::fs::read_to_string(env_root.join("launcher-portable.dat")).unwrap(),
            "portable_config_user_modified"
        );
        assert_eq!(
            std::fs::read_to_string(
                env_root.join("data").join("venv").join("pyvenv.cfg")
            )
            .unwrap(),
            "home = /usr/bin/python3"
        );
        assert_eq!(
            std::fs::read_to_string(
                env_root
                    .join("ComfyUI")
                    .join("models")
                    .join("big_model.safetensors")
            )
            .unwrap(),
            "FAKE_MODEL_CONTENT_4GB"
        );

        // pending 已被清空
        assert!(!pending.exists(), "pending 应该被清空");

        // 清理
        let _ = std::fs::remove_dir_all(&env_root);
    }

    #[test]
    fn test_apply_update_to_missing_staging() {
        let pending = temp_dir("e2e_pending_missing");
        let result = apply_update_to(
            std::path::Path::new("Z:/__nonexistent_path_zzz__"),
            &pending,
        );
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&pending);
    }

    #[test]
    fn test_apply_update_to_missing_exe() {
        // staging 缺少必需的 BoundLaunch.exe → 应该报错
        let staging = temp_dir("e2e_no_exe");
        std::fs::write(staging.join("BoundLaunch.dll"), b"dll").unwrap();
        let pending = temp_dir("e2e_no_exe_pending");

        let result = apply_update_to(&staging, &pending);
        assert!(result.is_err(), "缺少 BoundLaunch.exe 应该报错");

        let _ = std::fs::remove_dir_all(&staging);
        let _ = std::fs::remove_dir_all(&pending);
    }

    #[test]
    fn test_apply_pending_update_with_no_pending() {
        // 没有 pending 目录 → 直接返回（全 false），不报错
        let env_root = temp_dir("e2e_no_pending");
        let pending = std::env::temp_dir().join("updater_apply_test_e2e_no_pending_xxx");

        let result = apply_pending_update_with(&pending, &env_root);
        assert!(!result.exe_updated);
        assert!(!result.dll_updated);
        assert!(!result.uv_updated);

        let _ = std::fs::remove_dir_all(&env_root);
    }

    #[test]
    fn test_apply_pending_update_with_overwrites_existing() {
        // 已有 exe 被 .new 覆盖
        let pending = temp_dir("e2e_overwrite");
        let env_root = temp_dir("e2e_overwrite_env");

        std::fs::write(pending.join("BoundLaunch.exe.new"), b"NEW").unwrap();
        std::fs::write(env_root.join("BoundLaunch.exe"), b"OLD").unwrap();

        let result = apply_pending_update_with(&pending, &env_root);

        assert!(result.exe_updated);
        assert_eq!(
            std::fs::read_to_string(env_root.join("BoundLaunch.exe")).unwrap(),
            "NEW"
        );

        let _ = std::fs::remove_dir_all(&env_root);
    }
}
