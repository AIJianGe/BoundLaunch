//! Yaml 模板生成与备份策略
//!
//! 设计模式：纯函数 + Adapter
//! - `render_yaml_content`: 纯函数，相同 config 生成相同 yaml 内容
//! - `atomic_write`: 写 .tmp → rename，避免半截写入
//! - `backup_user_yaml`: 用户手动 yaml 自动备份为 .user-bak-<ts>
//!
//! 详见 `PR/03-模块设计/05-ModelPathManager.md §4.1 / §7 / §9.3`

use std::path::{Path, PathBuf};

use crate::config::{ModelsConfig, ModelsMode};

use super::models::ModelPathError;

/// ComfyUI 约定的 16 个模型子目录
///
/// 固定常量，不可配置（来自 ComfyUI 源码约定）
pub const COMFYUI_MODEL_SUBDIRS: &[&str] = &[
    "checkpoints",
    "vae",
    "loras",
    "embeddings",
    "controlnet",
    "clip",
    "clip_vision",
    "upscale_models",
    "diffusion_models",
    "gligen",
    "sam",
    "depth_anything",
    "animatediff_models",
    "vae_approx",
    "style_models",
    "unet",
];

/// 识别的模型文件扩展名
pub const MODEL_FILE_EXTENSIONS: &[&str] =
    &[".safetensors", ".ckpt", ".pt", ".bin", ".pth", ".gguf"];

/// launcher 生成 yaml 的首行标识
///
/// `is_launcher_generated()` 通过读首行匹配此字符串判断
pub const LAUNCHER_YAML_MARKER: &str = "# boundlaunch-launcher-generated-yaml v1";

/// 渲染 yaml 内容（纯函数）
///
/// 相同 `ModelsConfig` 必然生成相同字符串（同时间戳下）。
/// 注意：时间戳不属于"配置标识"，仅作人类可读注释。
pub fn render_yaml_content(config: &ModelsConfig) -> Result<String, ModelPathError> {
    if config.custom_root.as_os_str().is_empty() && matches!(config.mode, ModelsMode::CustomRoot) {
        return Err(ModelPathError::EmptyRoot);
    }

    // 路径统一使用正斜杠（ComfyUI 约定，跨平台兼容）
    let root = config
        .custom_root
        .to_string_lossy()
        .replace('\\', "/");

    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mode_str = match config.mode {
        ModelsMode::Default => "default",
        ModelsMode::CustomRoot => "custom_root",
        ModelsMode::Advanced => "advanced",
    };

    let mut s = String::with_capacity(512);
    s.push_str(LAUNCHER_YAML_MARKER);
    s.push('\n');
    s.push_str("# ============================================================\n");
    s.push_str("# 此文件由 无界启动器 launcher 自动生成，请勿手动修改\n");
    s.push_str(&format!("# 生成时间：{}\n", timestamp));
    s.push_str(&format!("# 配置模式：{}\n", mode_str));
    s.push_str(&format!("# 根目录：{}\n", root));
    s.push_str("# 切换回 default 模式时本文件会被自动删除\n");
    s.push_str("# ============================================================\n\n");

    // 16 个子目录映射
    for subdir in COMFYUI_MODEL_SUBDIRS {
        s.push_str(&format!("{}: {}/{}\n", subdir, root, subdir));
    }

    Ok(s)
}

/// 原子写入 yaml 文件
///
/// 流程：写 `.yaml.tmp` → rename 覆盖。
/// 防止写入中途崩溃导致 yaml 损坏（ComfyUI 读到半截 yaml 会报错）。
pub async fn atomic_write(path: &Path, content: &str) -> Result<(), ModelPathError> {
    let tmp = path.with_extension("yaml.tmp");

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    tokio::fs::write(&tmp, content).await?;
    tokio::fs::rename(&tmp, path).await?;

    Ok(())
}

/// 判断 yaml 是否由 launcher 生成
///
/// 仅读首行（最多 256 字节），< 5ms。
/// 不存在则返回 `Ok(false)`。
pub async fn is_launcher_generated(yaml_path: &Path) -> Result<bool, ModelPathError> {
    if !yaml_path.exists() {
        return Ok(false);
    }

    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(yaml_path).await?;
    let mut buf = [0u8; 256];
    let n = file.read(&mut buf).await.unwrap_or(0);

    if n == 0 {
        return Ok(false);
    }

    let head = std::str::from_utf8(&buf[..n]).unwrap_or("");
    Ok(head.contains(LAUNCHER_YAML_MARKER))
}

/// 备份用户手动配置的 yaml
///
/// - 不存在 → `Ok(None)`
/// - launcher 生成的 → `Ok(None)`（无需备份，可直接覆盖）
/// - 用户手动 yaml → 复制为 `<name>.yaml.user-bak-<ts>`，返回备份路径
pub async fn backup_user_yaml(yaml_path: &Path) -> Result<Option<PathBuf>, ModelPathError> {
    if !yaml_path.exists() {
        return Ok(None);
    }

    if is_launcher_generated(yaml_path).await? {
        return Ok(None);
    }

    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let backup = yaml_path.with_extension(format!("yaml.user-bak-{}", ts));

    tokio::fs::copy(yaml_path, &backup)
        .await
        .map_err(|_| ModelPathError::BackupFailed {
            src: yaml_path.to_path_buf(),
            dst: backup.clone(),
        })?;

    tracing::info!(?yaml_path, ?backup, "backed up user yaml");
    Ok(Some(backup))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample_config(root: &str) -> ModelsConfig {
        ModelsConfig {
            mode: ModelsMode::CustomRoot,
            custom_root: PathBuf::from(root),
            advanced: Default::default(),
        }
    }

    #[test]
    fn test_render_yaml_idempotent() {
        let cfg = sample_config("/data/models");
        let s1 = render_yaml_content(&cfg).unwrap();
        let s2 = render_yaml_content(&cfg).unwrap();
        // 16 子目录行必须一致（header 含时间戳可能不同秒）
        // 过滤掉注释行和空行（header 与 body 之间的空行不属于配置标识）
        let body1: Vec<&str> = s1
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        let body2: Vec<&str> = s2
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        assert_eq!(body1, body2);
        assert_eq!(body1.len(), COMFYUI_MODEL_SUBDIRS.len());
    }

    #[test]
    fn test_render_yaml_paths_use_forward_slash() {
        let cfg = sample_config(r"C:\data\models");
        let s = render_yaml_content(&cfg).unwrap();
        assert!(s.contains("C:/data/models/checkpoints"));
        assert!(!s.contains(r"C:\data"));
    }

    #[test]
    fn test_render_yaml_contains_all_subdirs() {
        let cfg = sample_config("/data/models");
        let s = render_yaml_content(&cfg).unwrap();
        for subdir in COMFYUI_MODEL_SUBDIRS {
            let line = format!("{}: /data/models/{}", subdir, subdir);
            assert!(s.contains(&line), "missing subdir line: {}", line);
        }
    }

    #[test]
    fn test_render_yaml_empty_root_error() {
        let mut cfg = sample_config("");
        cfg.mode = ModelsMode::CustomRoot;
        let result = render_yaml_content(&cfg);
        assert!(matches!(result, Err(ModelPathError::EmptyRoot)));
    }

    #[test]
    fn test_marker_is_first_line() {
        let cfg = sample_config("/data/models");
        let s = render_yaml_content(&cfg).unwrap();
        let first_line = s.lines().next().unwrap();
        assert_eq!(first_line, LAUNCHER_YAML_MARKER);
    }

    #[tokio::test]
    async fn test_atomic_write_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("extra_model_paths.yaml");
        let content = "checkpoints: /data/checkpoints\n";
        atomic_write(&target, content).await.unwrap();
        let read = tokio::fs::read_to_string(&target).await.unwrap();
        assert_eq!(read, content);
        assert!(!target.with_extension("yaml.tmp").exists());
    }

    #[tokio::test]
    async fn test_is_launcher_generated_marker_present() {
        let tmp = tempfile::tempdir().unwrap();
        let yaml = tmp.path().join("extra_model_paths.yaml");
        let cfg = sample_config("/data");
        let content = render_yaml_content(&cfg).unwrap();
        atomic_write(&yaml, &content).await.unwrap();
        assert!(is_launcher_generated(&yaml).await.unwrap());
    }

    #[tokio::test]
    async fn test_is_launcher_generated_user_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        let yaml = tmp.path().join("extra_model_paths.yaml");
        tokio::fs::write(&yaml, "# user manual yaml\ncheckpoints: /data\n")
            .await
            .unwrap();
        assert!(!is_launcher_generated(&yaml).await.unwrap());
    }

    #[tokio::test]
    async fn test_is_launcher_generated_nonexistent() {
        let yaml = Path::new("/nonexistent/path/extra_model_paths.yaml");
        assert!(!is_launcher_generated(yaml).await.unwrap());
    }

    #[tokio::test]
    async fn test_backup_user_yaml_skips_launcher_generated() {
        let tmp = tempfile::tempdir().unwrap();
        let yaml = tmp.path().join("extra_model_paths.yaml");
        let cfg = sample_config("/data");
        let content = render_yaml_content(&cfg).unwrap();
        atomic_write(&yaml, &content).await.unwrap();

        let result = backup_user_yaml(&yaml).await.unwrap();
        assert!(result.is_none(), "launcher-generated yaml 不应被备份");
    }

    #[tokio::test]
    async fn test_backup_user_yaml_backs_up_user_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        let yaml = tmp.path().join("extra_model_paths.yaml");
        tokio::fs::write(&yaml, "# user yaml\ncheckpoints: /x\n")
            .await
            .unwrap();

        let backup = backup_user_yaml(&yaml).await.unwrap();
        assert!(backup.is_some());
        let backup_path = backup.unwrap();
        assert!(backup_path.exists());
        assert!(backup_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("user-bak-"));
    }

    #[tokio::test]
    async fn test_backup_user_yaml_nonexistent() {
        let yaml = Path::new("/nonexistent/extra_model_paths.yaml");
        let result = backup_user_yaml(yaml).await.unwrap();
        assert!(result.is_none());
    }
}
