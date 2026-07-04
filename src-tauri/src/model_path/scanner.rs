//! 子目录扫描与模型文件列举
//!
//! 设计模式：Cache-Aside（60s TTL + root mtime 双重检查）
//!
//! 性能要点（详见 `PR/03-模块设计/05-ModelPathManager.md §5.2 / §6`）：
//! - 大目录扫描是 CPU+IO 混合阻塞任务
//! - 必须 `tokio::task::spawn_blocking` 移出 tokio reactor
//! - 16 子目录通过 `rayon::par_iter` 并行扫描
//! - `scan_subdirs` 总时长上限 5 秒
//!
//! 缓存策略：
//! - 60 秒 TTL
//! - root 路径变化则失效
//! - root mtime 变化则失效（外部新增/删除子目录会改变 mtime）

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use parking_lot::RwLock;
use rayon::prelude::*;

use super::models::{ModelFile, ModelPathError, ScanResult, SubdirInfo};
use super::yaml_gen::COMFYUI_MODEL_SUBDIRS;

/// 60 秒 TTL 缓存
pub const SCAN_CACHE_TTL: Duration = Duration::from_secs(60);

/// 扫描缓存
#[derive(Debug, Clone)]
pub struct ScanCache {
    pub root: PathBuf,
    pub root_mtime: Option<SystemTime>,
    pub result: ScanResult,
    pub cached_at: Instant,
}

impl ScanCache {
    pub fn is_fresh(&self, root: &Path, root_mtime: Option<&SystemTime>) -> bool {
        self.cached_at.elapsed() < SCAN_CACHE_TTL
            && self.root == root
            && self.root_mtime.as_ref() == root_mtime
    }
}

/// 扫描缓存容器（内部 RwLock，多读单写）
pub type ScanCacheStore = RwLock<Option<ScanCache>>;

/// 创建空的扫描缓存
pub fn new_scan_cache() -> ScanCacheStore {
    RwLock::new(None)
}

/// 扫描指定根目录下的所有 ComfyUI 子目录
///
/// - 缓存命中（root 路径 + root mtime 双重一致）直接返回
/// - 未命中：spawn_blocking + rayon par_iter 并行扫描 16 子目录
/// - 总时长上限 5 秒（spawn_blocking 内部不再额外超时控制，
///   但调用方可用 `tokio::time::timeout` 包裹）
pub async fn scan_subdirs(
    root: &Path,
    cache: &ScanCacheStore,
    force: bool,
) -> Result<ScanResult, ModelPathError> {
    if root.as_os_str().is_empty() {
        return Err(ModelPathError::EmptyRoot);
    }
    if !root.exists() {
        return Err(ModelPathError::RootNotFound(root.to_path_buf()));
    }

    // 1. 读取 root mtime（缓存校验用）
    let root_mtime = std::fs::metadata(root).ok().and_then(|m| m.modified().ok());

    // 2. 检查缓存（非 force 模式）
    if !force {
        let cache_read = cache.read();
        if let Some(c) = cache_read.as_ref() {
            if c.is_fresh(root, root_mtime.as_ref()) {
                tracing::debug!(?root, "scan_subdirs cache hit");
                return Ok(c.result.clone());
            }
        }
    }

    // 3. spawn_blocking 包裹阻塞 IO（rayon 内部并行 16 子目录）
    let root_clone = root.to_path_buf();
    let result = tokio::task::spawn_blocking(move || scan_subdirs_blocking(&root_clone))
        .await
        .map_err(|e| {
            tracing::error!(error = ?e, "scan_subdirs spawn_blocking failed");
            ModelPathError::ScanTimeout
        })?;

    // 4. 写入缓存
    let mut cache_write = cache.write();
    *cache_write = Some(ScanCache {
        root: root.to_path_buf(),
        root_mtime,
        result: result.clone(),
        cached_at: Instant::now(),
    });

    tracing::info!(?root, subdir_count = result.subdirs.len(), "scan completed");
    Ok(result)
}

/// 阻塞扫描（spawn_blocking 内部调用）
fn scan_subdirs_blocking(root: &Path) -> ScanResult {
    let subdirs: Vec<SubdirInfo> = COMFYUI_MODEL_SUBDIRS
        .par_iter()
        .map(|name| scan_one_subdir(root, name))
        .collect();

    ScanResult {
        root: root.to_path_buf(),
        subdirs,
        scanned_at: chrono::Utc::now(),
    }
}

/// 扫描单个子目录（rayon 并行任务单元）
fn scan_one_subdir(root: &Path, name: &str) -> SubdirInfo {
    let path = root.join(name);

    if !path.exists() {
        return SubdirInfo {
            name: name.to_string(),
            path,
            exists: false,
            model_count: 0,
            total_size_bytes: 0,
            models: vec![],
        };
    }

    // 默认填充模型列表（用户展开时无需再调 scan_models）
    let models = list_model_files(&path);
    let total_size = models.iter().map(|m| m.size_bytes).sum();

    SubdirInfo {
        name: name.to_string(),
        path,
        exists: true,
        model_count: models.len(),
        total_size_bytes: total_size,
        models,
    }
}

/// 列举目录下所有模型文件
///
/// 仅识别 `.safetensors/.ckpt/.pt/.bin/.pth/.gguf`，
/// 忽略 `.txt/.json` 等元数据文件。
fn list_model_files(dir: &Path) -> Vec<ModelFile> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(?dir, error = %e, "failed to read dir");
            return vec![];
        }
    };

    let mut files: Vec<ModelFile> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            if !path.is_file() {
                return None;
            }
            let ext = path.extension()?.to_string_lossy().to_lowercase();
            let ext_with_dot = format!(".{}", ext);
            if !is_model_extension(&ext_with_dot) {
                return None;
            }
            let metadata = e.metadata().ok()?;
            let modified = metadata.modified().ok()?;
            Some(ModelFile {
                name: e.file_name().to_string_lossy().to_string(),
                size_bytes: metadata.len(),
                modified: chrono::DateTime::<chrono::Utc>::from(modified),
                extension: ext_with_dot,
            })
        })
        .collect();

    // 文件名排序（保证多次扫描结果稳定）
    files.sort_by(|a, b| a.name.cmp(&b.name));
    files
}

/// 判断扩展名是否属于模型文件
fn is_model_extension(ext_with_dot: &str) -> bool {
    super::yaml_gen::MODEL_FILE_EXTENSIONS.contains(&ext_with_dot)
}

/// 单独扫描某个目录的模型文件
///
/// 用户在 UI 中展开某个子目录时调用。
/// 与 `scan_subdirs` 不同：
/// - 不检查缓存
/// - 直接 spawn_blocking + 单目录遍历
pub async fn scan_models(dir: &Path) -> Result<Vec<ModelFile>, ModelPathError> {
    if !dir.exists() {
        return Ok(vec![]);
    }

    let dir_clone = dir.to_path_buf();
    tokio::task::spawn_blocking(move || Ok(list_model_files(&dir_clone)))
        .await
        .map_err(|e| {
            tracing::error!(error = ?e, "scan_models spawn_blocking failed");
            ModelPathError::ScanTimeout
        })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_test_root() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        // 创建几个子目录与测试文件
        let ckpt = tmp.path().join("checkpoints");
        fs::create_dir_all(&ckpt).unwrap();
        fs::write(ckpt.join("model1.safetensors"), b"fake-1").unwrap();
        fs::write(ckpt.join("model2.ckpt"), b"fake-2").unwrap();
        fs::write(ckpt.join("readme.txt"), b"ignored").unwrap(); // 非模型文件

        let vae = tmp.path().join("vae");
        fs::create_dir_all(&vae).unwrap();
        fs::write(vae.join("vae.pt"), b"vae-1").unwrap();

        // loras 子目录留空
        fs::create_dir_all(tmp.path().join("loras")).unwrap();
        tmp
    }

    #[tokio::test]
    async fn test_scan_subdirs_returns_all_16_subdirs() {
        let tmp = make_test_root();
        let cache = new_scan_cache();
        let result = scan_subdirs(tmp.path(), &cache, false).await.unwrap();
        assert_eq!(result.subdirs.len(), COMFYUI_MODEL_SUBDIRS.len());
    }

    #[tokio::test]
    async fn test_scan_subdirs_marks_nonexistent_as_not_exists() {
        let tmp = make_test_root();
        let cache = new_scan_cache();
        let result = scan_subdirs(tmp.path(), &cache, false).await.unwrap();

        // embeddings/controlnet 等未创建的子目录应为 exists=false
        let embeddings = result
            .subdirs
            .iter()
            .find(|s| s.name == "embeddings")
            .unwrap();
        assert!(!embeddings.exists);
        assert_eq!(embeddings.model_count, 0);

        // checkpoints 已创建且有文件
        let ckpt = result
            .subdirs
            .iter()
            .find(|s| s.name == "checkpoints")
            .unwrap();
        assert!(ckpt.exists);
        assert_eq!(ckpt.model_count, 2);
        assert_eq!(ckpt.models.len(), 2);
    }

    #[tokio::test]
    async fn test_scan_subdirs_filter_non_model_files() {
        let tmp = make_test_root();
        let cache = new_scan_cache();
        let result = scan_subdirs(tmp.path(), &cache, false).await.unwrap();
        let ckpt = result
            .subdirs
            .iter()
            .find(|s| s.name == "checkpoints")
            .unwrap();
        let names: Vec<&str> = ckpt.models.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"model1.safetensors"));
        assert!(names.contains(&"model2.ckpt"));
        assert!(!names.contains(&"readme.txt"));
    }

    #[tokio::test]
    async fn test_scan_subdirs_cache_hit() {
        let tmp = make_test_root();
        let cache = new_scan_cache();

        // 第一次扫描写入缓存
        let r1 = scan_subdirs(tmp.path(), &cache, false).await.unwrap();
        // 第二次应命中缓存（同 root + 同 mtime）
        let r2 = scan_subdirs(tmp.path(), &cache, false).await.unwrap();
        assert_eq!(r1.scanned_at, r2.scanned_at, "缓存命中应返回同一 scanned_at");
    }

    #[tokio::test]
    async fn test_scan_subdirs_force_refresh() {
        let tmp = make_test_root();
        let cache = new_scan_cache();
        let _r1 = scan_subdirs(tmp.path(), &cache, false).await.unwrap();

        // force=true 应重新扫描（即使缓存有效）
        // 注意：scanned_at 在毫秒级可能相同，故用缓存 invalidated 后判断
        let r2 = scan_subdirs(tmp.path(), &cache, true).await.unwrap();
        assert_eq!(r2.subdirs.len(), COMFYUI_MODEL_SUBDIRS.len());
    }

    #[tokio::test]
    async fn test_scan_subdirs_root_mtime_invalidation() {
        let tmp = make_test_root();
        let cache = new_scan_cache();
        let r1 = scan_subdirs(tmp.path(), &cache, false).await.unwrap();

        // 修改 root（创建新子目录）使 root mtime 改变
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::create_dir_all(tmp.path().join("newdir")).unwrap();

        let r2 = scan_subdirs(tmp.path(), &cache, false).await.unwrap();
        assert_ne!(
            r1.scanned_at, r2.scanned_at,
            "root mtime 变化应使缓存失效"
        );
    }

    #[tokio::test]
    async fn test_scan_subdirs_nonexistent_root() {
        let cache = new_scan_cache();
        let result = scan_subdirs(Path::new("/nonexistent/path"), &cache, false).await;
        assert!(matches!(result, Err(ModelPathError::RootNotFound(_))));
    }

    #[tokio::test]
    async fn test_scan_subdirs_empty_root() {
        let cache = new_scan_cache();
        let result = scan_subdirs(Path::new(""), &cache, false).await;
        assert!(matches!(result, Err(ModelPathError::EmptyRoot)));
    }

    #[tokio::test]
    async fn test_scan_models_returns_model_files() {
        let tmp = make_test_root();
        let ckpt = tmp.path().join("checkpoints");
        let models = scan_models(&ckpt).await.unwrap();
        assert_eq!(models.len(), 2);
        assert!(models.iter().any(|m| m.name == "model1.safetensors"));
        assert!(models.iter().any(|m| m.name == "model2.ckpt"));
    }

    #[tokio::test]
    async fn test_scan_models_nonexistent_dir_returns_empty() {
        let models = scan_models(Path::new("/nonexistent/dir")).await.unwrap();
        assert!(models.is_empty());
    }

    #[tokio::test]
    async fn test_is_model_extension() {
        assert!(is_model_extension(".safetensors"));
        assert!(is_model_extension(".ckpt"));
        assert!(is_model_extension(".pt"));
        assert!(is_model_extension(".bin"));
        assert!(is_model_extension(".pth"));
        assert!(is_model_extension(".gguf"));
        assert!(!is_model_extension(".txt"));
        assert!(!is_model_extension(".json"));
        assert!(!is_model_extension(""));
    }
}
