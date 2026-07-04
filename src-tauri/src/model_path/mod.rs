//! ModelPathManager 模块
//!
//! 详见 `PR/03-模块设计/05-ModelPathManager.md`
//!
//! ## 职责
//! - 根据 Config 生成 ComfyUI 的 `extra_model_paths.yaml`
//! - 模式切换时删除 yaml（恢复 ComfyUI 默认行为）
//! - 备份用户手动配置的 yaml
//! - 校验根目录合法性
//! - 扫描子目录与模型文件
//! - 启动前自动生成 yaml（被 ProcessLauncher 调用）
//!
//! ## 设计模式
//! - **Cache-Aside**：扫描结果 60s TTL + root mtime 双重检查
//! - **Adapter**：yaml_gen 封装 yaml 模板；scanner 封装文件系统遍历
//! - **Template Method**：ensure_yaml_for_launch 按 mode 分发
//! - **Facade**：ModelPathService 作为对外统一入口

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::config::{ModelsConfig, ModelsMode};

pub mod models;
pub mod scanner;
pub mod yaml_gen;

pub use models::{GenerateYamlResult, ModelFile, ModelPathError, ScanResult};
pub use scanner::{new_scan_cache, ScanCacheStore};
pub use yaml_gen::{
    atomic_write, backup_user_yaml, is_launcher_generated, render_yaml_content,
};

/// 校验根目录合法性（无状态，供 commands 直接调用）
///
/// 检查项：
/// - 空路径 → `EmptyRoot`
/// - 不存在 → `RootNotFound`
/// - 不可读 → `RootNotReadable`
///
/// 注意：与 `comfyui_root` 重复的情况不在此处报错，
/// 由调用方根据 `ModelPathService::is_same_as_comfyui` 自行处理
/// （设计文档 §10 "警告但允许"）。
pub fn validate_root(path: &Path) -> Result<(), ModelPathError> {
    if path.as_os_str().is_empty() {
        return Err(ModelPathError::EmptyRoot);
    }
    if !path.exists() {
        return Err(ModelPathError::RootNotFound(path.to_path_buf()));
    }
    if std::fs::metadata(path).is_err() {
        return Err(ModelPathError::RootNotReadable(path.to_path_buf()));
    }
    Ok(())
}

/// 模型路径管理服务
///
/// 设计模式：
/// - **单例**：通过 `AppState` 全局共享（Arc 包裹）
/// - **Mutex**：`yaml_lock` 串行化 yaml 写操作（generate / remove 互斥）
/// - **RwLock**：`scan_cache` 多读单写，提升扫描并发性能
pub struct ModelPathService {
    /// ComfyUI 根目录（用于 RootSameAsComfyui 校验）
    comfyui_root: PathBuf,
    /// `<comfyui_root>/extra_model_paths.yaml`
    yaml_path: PathBuf,
    /// 扫描缓存（60s TTL + root mtime 检查）
    scan_cache: Arc<ScanCacheStore>,
    /// yaml 写入互斥锁（generate / remove 串行化）
    yaml_lock: Mutex<()>,
}

impl ModelPathService {
    /// 创建服务实例
    ///
    /// `comfyui_root` 用于：
    /// 1. 拼接 `extra_model_paths.yaml` 路径
    /// 2. 校验 custom_root 是否与之重复
    pub fn new(comfyui_root: PathBuf) -> Self {
        let yaml_path = comfyui_root.join("extra_model_paths.yaml");
        Self {
            comfyui_root,
            yaml_path,
            scan_cache: Arc::new(new_scan_cache()),
            yaml_lock: Mutex::new(()),
        }
    }

    /// 生成 `extra_model_paths.yaml`
    ///
    /// 仅 `ModelsMode::CustomRoot` 模式生效。
    /// 流程：
    /// 1. 加 yaml_lock
    /// 2. validate_root
    /// 3. 渲染 yaml（纯函数）
    /// 4. 备份用户手动 yaml（如有）
    /// 5. 原子写入（写 .tmp → rename）
    pub async fn generate_yaml(
        &self,
        models_config: &ModelsConfig,
    ) -> Result<GenerateYamlResult, ModelPathError> {
        let _guard = self.yaml_lock.lock().await;

        // 仅 custom_root 模式生成 yaml
        if !matches!(models_config.mode, ModelsMode::CustomRoot) {
            return Err(ModelPathError::InvalidPath(format!(
                "mode {:?} 不支持 generate_yaml，仅 custom_root 模式生效",
                models_config.mode
            )));
        }

        // 1. 验证根目录
        validate_root(&models_config.custom_root)?;

        // 2. 渲染 yaml 内容（纯函数 - 相同 config 相同输出）
        let content = render_yaml_content(models_config)?;
        tracing::info!(
            ?self.yaml_path,
            root = ?models_config.custom_root,
            "generating extra_model_paths.yaml"
        );

        // 3. 备份用户手动 yaml（launcher 生成的无需备份）
        let backed_up = backup_user_yaml(&self.yaml_path).await?;

        // 4. 原子写入
        atomic_write(&self.yaml_path, &content).await?;

        tracing::info!(?self.yaml_path, ?backed_up, "yaml generated");
        Ok(GenerateYamlResult {
            yaml_path: self.yaml_path.clone(),
            backed_up,
            generated_at: chrono::Utc::now(),
        })
    }

    /// 删除 launcher 生成的 yaml
    ///
    /// - yaml 不存在 → `Ok(())`（幂等）
    /// - launcher 生成的 → 删除
    /// - 用户手动 yaml → 跳过（不删除）
    pub async fn remove_yaml(&self) -> Result<(), ModelPathError> {
        let _guard = self.yaml_lock.lock().await;

        if !self.yaml_path.exists() {
            return Ok(());
        }

        if !is_launcher_generated(&self.yaml_path).await? {
            tracing::warn!(
                ?self.yaml_path,
                "skipped removal: yaml is not launcher-generated"
            );
            return Ok(());
        }

        tokio::fs::remove_file(&self.yaml_path).await?;
        tracing::info!(?self.yaml_path, "removed launcher-generated yaml");
        Ok(())
    }

    /// 校验根目录合法性
    ///
    /// 检查项：
    /// - 空路径 → `EmptyRoot`
    /// - 不存在 → `RootNotFound`
    /// - 不可读 → `RootNotReadable`
    ///
    /// 注意：与 `comfyui_root` 重复的情况不在此处报错，
    /// 由调用方根据 `is_same_as_comfyui` 自行处理（设计文档 §10 "警告但允许"）。
    pub async fn validate_root(&self, path: &Path) -> Result<(), ModelPathError> {
        // 委托给模块级无状态函数（保持 API 兼容）
        validate_root(path)
    }

    /// 判断 custom_root 是否与 comfyui_root 重复
    ///
    /// 重复时调用方应向用户显示警告（但允许继续）。
    pub fn is_same_as_comfyui(&self, custom_root: &Path) -> bool {
        // 标准化比较（不解析符号链接，避免 IO）
        let a = self.comfyui_root.canonicalize().unwrap_or_else(|_| self.comfyui_root.clone());
        let b = custom_root.canonicalize().unwrap_or_else(|_| custom_root.to_path_buf());
        a == b
    }

    /// 扫描根目录下所有 ComfyUI 子目录
    ///
    /// 详见 `scanner::scan_subdirs`（spawn_blocking + rayon par_iter 并行扫描 16 子目录）。
    pub async fn scan_subdirs(
        &self,
        root: &Path,
        force: bool,
    ) -> Result<ScanResult, ModelPathError> {
        scanner::scan_subdirs(root, &self.scan_cache, force).await
    }

    /// 单独扫描某个目录的模型文件
    ///
    /// 用户在 UI 中展开某个子目录时调用。
    pub async fn scan_models(&self, dir: &Path) -> Result<Vec<ModelFile>, ModelPathError> {
        scanner::scan_models(dir).await
    }

    /// 判断 yaml 是否由 launcher 生成
    ///
    /// 仅读首行（< 5ms）。
    pub async fn is_launcher_generated(&self) -> Result<bool, ModelPathError> {
        is_launcher_generated(&self.yaml_path).await
    }

    /// 启动前确保 yaml 状态正确（被 ProcessLauncher 调用）
    ///
    /// 按 `ModelsConfig.mode` 分发：
    /// - `Default` 模式：launcher 生成的 yaml 删除（恢复 ComfyUI 默认行为）；用户手动 yaml 不动
    /// - `CustomRoot` 模式：validate_root + 生成 yaml
    /// - `Advanced` 模式：本期按 default 处理
    ///
    /// 幂等性：相同 config 多次调用结果一致。
    pub async fn ensure_yaml_for_launch(
        &self,
        models_config: &ModelsConfig,
    ) -> Result<(), ModelPathError> {
        match models_config.mode {
            ModelsMode::Default | ModelsMode::Advanced => {
                // 删除 launcher 自己生成的 yaml（用户手动的保留）
                if self.is_launcher_generated().await? {
                    self.remove_yaml().await?;
                }
                Ok(())
            }
            ModelsMode::CustomRoot => {
                // 生成 yaml（内部含 validate_root + 备份 + 原子写入）
                self.generate_yaml(models_config).await?;
                Ok(())
            }
        }
    }

    /// yaml 文件路径（供调试 / 测试）
    pub fn yaml_path(&self) -> &Path {
        &self.yaml_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_service(tmp: &std::path::Path) -> ModelPathService {
        let comfyui_root = tmp.join("comfyui");
        std::fs::create_dir_all(&comfyui_root).unwrap();
        ModelPathService::new(comfyui_root)
    }

    fn custom_root_config(root: &Path) -> ModelsConfig {
        ModelsConfig {
            mode: ModelsMode::CustomRoot,
            custom_root: root.to_path_buf(),
            advanced: Default::default(),
        }
    }

    #[tokio::test]
    async fn test_generate_yaml_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());

        let models_root = tmp.path().join("models");
        std::fs::create_dir_all(&models_root).unwrap();
        let cfg = custom_root_config(&models_root);

        let result = svc.generate_yaml(&cfg).await.unwrap();
        assert!(result.yaml_path.exists());
        assert!(result.backed_up.is_none()); // 无旧 yaml
        assert!(svc.is_launcher_generated().await.unwrap());
    }

    #[tokio::test]
    async fn test_generate_yaml_overwrites_launcher_generated() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());

        // 第一次生成
        let models_root_1 = tmp.path().join("models1");
        std::fs::create_dir_all(&models_root_1).unwrap();
        let cfg1 = custom_root_config(&models_root_1);
        let r1 = svc.generate_yaml(&cfg1).await.unwrap();
        assert!(r1.backed_up.is_none());

        // 第二次生成（覆盖 launcher 自己的 yaml，不备份）
        let models_root_2 = tmp.path().join("models2");
        std::fs::create_dir_all(&models_root_2).unwrap();
        let cfg2 = custom_root_config(&models_root_2);
        let r2 = svc.generate_yaml(&cfg2).await.unwrap();
        assert!(r2.backed_up.is_none(), "launcher 生成的 yaml 覆盖不应备份");

        // 验证内容是新的 root
        let content = tokio::fs::read_to_string(&svc.yaml_path).await.unwrap();
        assert!(content.contains("models2/checkpoints"));
    }

    #[tokio::test]
    async fn test_generate_yaml_backs_up_user_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());

        // 用户手动 yaml
        let user_yaml = "# user manual yaml\ncheckpoints: /old/data\n";
        tokio::fs::write(&svc.yaml_path, user_yaml).await.unwrap();

        let models_root = tmp.path().join("models");
        std::fs::create_dir_all(&models_root).unwrap();
        let cfg = custom_root_config(&models_root);

        let result = svc.generate_yaml(&cfg).await.unwrap();
        assert!(result.backed_up.is_some(), "用户 yaml 必须备份");

        // 验证备份文件存在
        let backup = result.backed_up.unwrap();
        assert!(backup.exists());
        let backup_content = tokio::fs::read_to_string(&backup).await.unwrap();
        assert_eq!(backup_content, user_yaml);

        // 验证当前 yaml 是 launcher 生成的
        assert!(svc.is_launcher_generated().await.unwrap());
    }

    #[tokio::test]
    async fn test_remove_yaml_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());

        // 不存在时返回 Ok
        svc.remove_yaml().await.unwrap();

        // 生成后再删
        let models_root = tmp.path().join("models");
        std::fs::create_dir_all(&models_root).unwrap();
        let cfg = custom_root_config(&models_root);
        svc.generate_yaml(&cfg).await.unwrap();
        assert!(svc.yaml_path.exists());

        svc.remove_yaml().await.unwrap();
        assert!(!svc.yaml_path.exists());

        // 重复删除仍 Ok
        svc.remove_yaml().await.unwrap();
    }

    #[tokio::test]
    async fn test_remove_yaml_skips_user_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());

        // 用户手动 yaml
        tokio::fs::write(&svc.yaml_path, "# user yaml\ncheckpoints: /x\n")
            .await
            .unwrap();

        svc.remove_yaml().await.unwrap();
        assert!(svc.yaml_path.exists(), "用户 yaml 不应被删除");
    }

    #[tokio::test]
    async fn test_validate_root_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());
        let result = svc.validate_root(Path::new("/nonexistent/path")).await;
        assert!(matches!(result, Err(ModelPathError::RootNotFound(_))));
    }

    #[tokio::test]
    async fn test_validate_root_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());
        let result = svc.validate_root(Path::new("")).await;
        assert!(matches!(result, Err(ModelPathError::EmptyRoot)));
    }

    #[tokio::test]
    async fn test_validate_root_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());
        let dir = tmp.path().join("models");
        std::fs::create_dir_all(&dir).unwrap();
        svc.validate_root(&dir).await.unwrap();
    }

    #[tokio::test]
    async fn test_is_same_as_comfyui() {
        let tmp = tempfile::tempdir().unwrap();
        let comfyui_root = tmp.path().join("comfyui");
        std::fs::create_dir_all(&comfyui_root).unwrap();
        let svc = ModelPathService::new(comfyui_root.clone());

        assert!(svc.is_same_as_comfyui(&comfyui_root));
        assert!(!svc.is_same_as_comfyui(&tmp.path().join("other")));
    }

    #[tokio::test]
    async fn test_ensure_yaml_for_launch_default_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());

        // 先生成 launcher yaml
        let models_root = tmp.path().join("models");
        std::fs::create_dir_all(&models_root).unwrap();
        let cfg_custom = custom_root_config(&models_root);
        svc.generate_yaml(&cfg_custom).await.unwrap();
        assert!(svc.yaml_path.exists());

        // 切回 default 模式 → 删除 launcher yaml
        let cfg_default = ModelsConfig {
            mode: ModelsMode::Default,
            custom_root: PathBuf::new(),
            advanced: Default::default(),
        };
        svc.ensure_yaml_for_launch(&cfg_default).await.unwrap();
        assert!(!svc.yaml_path.exists(), "default 模式应删除 launcher yaml");
    }

    #[tokio::test]
    async fn test_ensure_yaml_for_launch_default_keeps_user_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());

        // 用户手动 yaml
        tokio::fs::write(&svc.yaml_path, "# user yaml\ncheckpoints: /x\n")
            .await
            .unwrap();

        let cfg_default = ModelsConfig {
            mode: ModelsMode::Default,
            custom_root: PathBuf::new(),
            advanced: Default::default(),
        };
        svc.ensure_yaml_for_launch(&cfg_default).await.unwrap();
        assert!(svc.yaml_path.exists(), "用户 yaml 应保留");
    }

    #[tokio::test]
    async fn test_ensure_yaml_for_launch_custom_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());

        let models_root = tmp.path().join("models");
        std::fs::create_dir_all(&models_root).unwrap();
        let cfg = custom_root_config(&models_root);

        svc.ensure_yaml_for_launch(&cfg).await.unwrap();
        assert!(svc.yaml_path.exists());
        assert!(svc.is_launcher_generated().await.unwrap());
    }

    #[tokio::test]
    async fn test_ensure_yaml_for_launch_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = make_service(tmp.path());

        let models_root = tmp.path().join("models");
        std::fs::create_dir_all(&models_root).unwrap();
        let cfg = custom_root_config(&models_root);

        // 多次调用结果一致
        svc.ensure_yaml_for_launch(&cfg).await.unwrap();
        let content1 = tokio::fs::read_to_string(&svc.yaml_path).await.unwrap();

        svc.ensure_yaml_for_launch(&cfg).await.unwrap();
        let content2 = tokio::fs::read_to_string(&svc.yaml_path).await.unwrap();

        // 时间戳在 header 中，但子目录映射行必须一致
        let body1: Vec<&str> = content1.lines().filter(|l| !l.starts_with('#') && !l.is_empty()).collect();
        let body2: Vec<&str> = content2.lines().filter(|l| !l.starts_with('#') && !l.is_empty()).collect();
        assert_eq!(body1, body2);
    }
}
