//! ModelPathManager 数据模型
//!
//! 详见 `PR/03-模块设计/05-ModelPathManager.md §3 接口签名`

use std::path::PathBuf;
use serde::Serialize;

/// 扫描结果
#[derive(Debug, Clone, Serialize)]
pub struct ScanResult {
    pub root: PathBuf,
    pub subdirs: Vec<SubdirInfo>,
    pub scanned_at: chrono::DateTime<chrono::Utc>,
}

/// 子目录信息
#[derive(Debug, Clone, Serialize)]
pub struct SubdirInfo {
    /// 子目录名（如 "checkpoints"）
    pub name: String,
    pub path: PathBuf,
    pub exists: bool,
    pub model_count: usize,
    pub total_size_bytes: u64,
    /// 仅在用户展开时填充
    pub models: Vec<ModelFile>,
}

/// 模型文件信息
#[derive(Debug, Clone, Serialize)]
pub struct ModelFile {
    pub name: String,
    pub size_bytes: u64,
    pub modified: chrono::DateTime<chrono::Utc>,
    pub extension: String,
}

/// generate_yaml 调用结果
#[derive(Debug, Clone, Serialize)]
pub struct GenerateYamlResult {
    pub yaml_path: PathBuf,
    /// 备份的原 yaml 路径（如有）
    pub backed_up: Option<PathBuf>,
    pub generated_at: chrono::DateTime<chrono::Utc>,
}

/// ModelPath 错误类型
///
/// 详见 `PR/03-模块设计/05-ModelPathManager.md §4.3 错误类型`
#[derive(Debug, thiserror::Error)]
pub enum ModelPathError {
    #[error("根目录路径为空")]
    EmptyRoot,
    #[error("根目录不存在: {0}")]
    RootNotFound(PathBuf),
    #[error("根目录不可读: {0}")]
    RootNotReadable(PathBuf),
    #[error("根目录与 ComfyUI 自带目录重复: {0}")]
    RootSameAsComfyui(PathBuf),
    #[error("路径含非法字符: {0}")]
    InvalidPath(String),
    #[error("yaml 写入失败: {0}")]
    YamlWriteError(#[from] std::io::Error),
    #[error("备份失败: 源 {src} 目标 {dst}")]
    BackupFailed { src: PathBuf, dst: PathBuf },
    #[error("扫描超时")]
    ScanTimeout,
}

impl From<ModelPathError> for String {
    fn from(e: ModelPathError) -> Self {
        e.to_string()
    }
}
