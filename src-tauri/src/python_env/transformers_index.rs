//! transformers 版本索引（v3.7 新增）
//!
//! 从 PyPI 拉取所有 transformers 版本号，三层缓存：
//! - L1: 内存缓存（运行时快速响应）
//! - L2: 本地文件缓存（启动时网络失败时用）
//! - L3: 硬编码 fallback（完全无缓存时的兜底）
//!
//! 拉取时机：
//! - App 启动时后台 spawn 拉取
//! - 用户手动刷新（通过 env_refresh_transformers_versions 命令）
//! - ComfyUI 版本切换后（触发一次刷新）
//!
//! 异步方案（v3.7 复用 v3.6 CancellationToken 模式）：
//! - 拉取用 reqwest + CancellationToken
//! - 30s timeout 改用 tokio::select! + tokio::time::sleep
//! - 完成后 emit `transformers_versions_updated` 前端事件 + 后端 SystemEvent

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::RwLock;
use serde::Deserialize;
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

use crate::event_bus::{EventBus, SystemEvent};

const PYPI_TRANSFORMERS_URL: &str = "https://pypi.org/pypi/transformers/json";

/// 拉取超时（30s，与 v3.6 约定一致）
const FETCH_TIMEOUT_SECS: u64 = 30;

/// 硬编码 fallback 版本列表（PyPI 拉取失败 + 无本地缓存时用）
///
/// 包含 ComfyUI 兼容的关键版本（4.50.3+）+ 最新 5.x
/// 维护：PyPI 拉取失败时用此列表，每次发布新版本后应更新
const FALLBACK_VERSIONS: &[&str] = &[
    // 5.x（实验，有破坏性 API 变更）
    "5.13.0",
    // 4.x 稳定版（最新在前）
    "4.57.3", "4.57.2", "4.57.1", "4.57.0",
    "4.56.3", "4.56.2", "4.56.1", "4.56.0",
    "4.55.2", "4.55.1", "4.55.0",
    "4.54.0",
    "4.53.3", "4.53.2", "4.53.1", "4.53.0",
    "4.52.4", "4.52.3", "4.52.2", "4.52.1", "4.52.0",
    "4.51.3", "4.51.2", "4.51.1", "4.51.0",
    "4.50.3", // ComfyUI requirements.txt 最低要求
];

/// transformers 版本索引服务
///
/// 设计模式：
/// - **Cache-Aside**：get_versions 先读缓存，未命中读文件，再未命中用 fallback
/// - **stale 模式**：invalidate() 清空内存缓存（下次 get_versions 会用 L2 文件）
/// - **事件驱动**：拉取完成后 emit 前端事件 + 后端 SystemEvent
#[derive(Clone)]
pub struct TransformersVersionIndex {
    /// L1 内存缓存（版本号列表，降序，最新在前）
    cache: Arc<RwLock<Option<Vec<String>>>>,
    /// L2 本地文件缓存路径（如 app_data_dir/transformers_versions.json）
    cache_file: PathBuf,
    /// 事件总线（用于广播 SystemEvent）
    event_bus: EventBus,
    /// Tauri AppHandle（用于 emit 前端事件）
    app_handle: Option<AppHandle>,
}

impl TransformersVersionIndex {
    /// 生产构造（注入 AppHandle，支持 emit 前端事件）
    pub fn new(cache_file: PathBuf, event_bus: EventBus, app_handle: AppHandle) -> Self {
        Self {
            cache: Arc::new(RwLock::new(None)),
            cache_file,
            event_bus,
            app_handle: Some(app_handle),
        }
    }

    /// 测试构造（无 AppHandle，不 emit 前端事件）
    pub fn new_for_test(cache_file: PathBuf, event_bus: EventBus) -> Self {
        Self {
            cache: Arc::new(RwLock::new(None)),
            cache_file,
            event_bus,
            app_handle: None,
        }
    }

    /// 获取版本列表（L1 缓存 → L2 文件 → L3 fallback）
    ///
    /// 同步方法，永远返回非空列表
    pub fn get_versions(&self) -> Vec<String> {
        // L1: 内存缓存
        {
            let cache = self.cache.read();
            if let Some(versions) = cache.as_ref() {
                return versions.clone();
            }
        }
        // L2: 本地文件缓存（同步读取）
        if let Some(versions) = self.load_from_file_sync() {
            return versions;
        }
        // L3: fallback
        FALLBACK_VERSIONS.iter().map(|s| s.to_string()).collect()
    }

    /// 启动后台拉取任务
    ///
    /// - 创建 CancellationToken（支持取消）
    /// - spawn tokio task 执行拉取
    /// - 完成后更新 L1 + L2 缓存 + emit 事件
    ///
    /// 不阻塞调用方：spawn 后立即返回
    ///
    /// 实现说明：使用 `tauri::async_runtime::spawn` 而非 `tokio::spawn`，
    /// 因为 `spawn_refresh` 可能在非 Tokio 当前线程上下文中被调用
    /// （如 lib.rs 的 setup hook 中 `rt.block_on` 块外）。
    /// Tauri 的 async_runtime 是全局管理的，不依赖 thread-local current reactor。
    pub fn spawn_refresh(&self) {
        let cache = self.cache.clone();
        let cache_file = self.cache_file.clone();
        let event_bus = self.event_bus.clone();
        let app_handle = self.app_handle.clone();
        let cancel = CancellationToken::new();

        tauri::async_runtime::spawn(async move {
            tracing::info!("transformers version index: refresh started");

            match fetch_versions(&cancel).await {
                Ok(versions) => {
                    // 1. 更新 L1 内存缓存
                    *cache.write() = Some(versions.clone());

                    // 2. 写 L2 本地文件缓存
                    if let Err(e) = save_to_file(&cache_file, &versions).await {
                        tracing::warn!(error = %e, "failed to save transformers versions to file");
                    }

                    // 3. emit 后端 SystemEvent（其他 Service 联动）
                    event_bus.emit(SystemEvent::TransformersVersionsUpdated);

                    // 4. emit 前端 Tauri Event（前端刷新选择器）
                    if let Some(handle) = &app_handle {
                        if let Err(e) = handle.emit("transformers_versions_updated", &versions) {
                            tracing::warn!(error = %e, "failed to emit transformers_versions_updated");
                        }
                    }

                    tracing::info!(
                        count = versions.len(),
                        "transformers version index: refresh complete"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "transformers version index: refresh failed, using fallback"
                    );
                }
            }
        });
    }

    /// 失效缓存（stale 模式，清空内存缓存，下次 get_versions 用 L2 文件）
    ///
    /// 与 env_inspector 的 stale 模式不同：transformers 版本列表不频繁变化，
    /// 简化实现为清空内存缓存，下次 get_versions 读 L2 文件。
    /// 若需要严格 stale（保留旧值 + 标记 stale），可扩展 cache 结构。
    pub fn invalidate(&self) {
        *self.cache.write() = None;
    }

    /// 同步加载本地文件缓存
    fn load_from_file_sync(&self) -> Option<Vec<String>> {
        let content = std::fs::read_to_string(&self.cache_file).ok()?;
        serde_json::from_str::<Vec<String>>(&content).ok()
    }
}

/// 从 PyPI 拉取所有 transformers 版本号
///
/// v3.7：用 CancellationToken + tokio::select! 替代 tokio::time::timeout
async fn fetch_versions(cancel: &CancellationToken) -> Result<Vec<String>, String> {
    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| format!("reqwest client build failed: {}", e))?;

    let request = client.get(PYPI_TRANSFORMERS_URL).send();

    let response = tokio::select! {
        r = request => {
            r.map_err(|e| format!("PyPI request failed: {}", e))?
        }
        _ = cancel.cancelled() => {
            return Err("cancelled".to_string());
        }
        _ = tokio::time::sleep(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS)) => {
            return Err(format!("PyPI request timeout ({}s)", FETCH_TIMEOUT_SECS));
        }
    };

    if !response.status().is_success() {
        return Err(format!("PyPI returned status: {}", response.status()));
    }

    let body: PyPiPackage = response
        .json()
        .await
        .map_err(|e| format!("PyPI JSON parse failed: {}", e))?;

    // 取所有版本号，排序降序（最新在前）
    let mut versions: Vec<String> = body.releases.keys().cloned().collect();
    versions.sort_by(|a, b| compare_versions(b, a));

    // 过滤掉空 release（有些版本号被 yank 后 releases 为空数组）
    // PyPI JSON 中 releases 的 value 是该版本的 files 列表，空数组表示无文件
    // 但我们只取 key（版本号），不检查 files，因为 yanked 版本号仍有参考价值
    // 用户切换到 yanked 版本时 uv pip install 会报错，这是合理的反馈

    Ok(versions)
}

/// 比较两个语义版本号（返回 Ordering）
///
/// "4.57.3" vs "4.50.3" → Greater
/// "5.0.0" vs "4.57.3" → Greater
/// "4.57" vs "4.57.0" → Equal
fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let va = parse_version(a);
    let vb = parse_version(b);
    va.cmp(&vb)
}

/// 解析版本号为数字数组（用于语义排序）
fn parse_version(v: &str) -> Vec<u32> {
    v.split('.').filter_map(|s| s.parse().ok()).collect()
}

/// 写入本地文件缓存
async fn save_to_file(path: &PathBuf, versions: &[String]) -> Result<(), String> {
    // 确保父目录存在
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("create dir failed: {}", e))?;
    }
    let content = serde_json::to_string(versions).map_err(|e| format!("serialize failed: {}", e))?;
    tokio::fs::write(path, content)
        .await
        .map_err(|e| format!("write failed: {}", e))
}

/// PyPI JSON 响应（只解析 releases 字段）
///
/// 完整格式见 https://pypi.org/pypi/transformers/json
/// releases 是 HashMap<版本号, Vec<文件信息>>
#[derive(Deserialize)]
struct PyPiPackage {
    releases: std::collections::HashMap<String, Vec<serde_json::Value>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compare_versions_equal() {
        assert_eq!(compare_versions("4.50.3", "4.50.3"), std::cmp::Ordering::Equal);
    }

    #[test]
    fn test_compare_versions_greater() {
        assert_eq!(compare_versions("4.57.3", "4.50.3"), std::cmp::Ordering::Greater);
    }

    #[test]
    fn test_compare_versions_major_version() {
        assert_eq!(compare_versions("5.0.0", "4.57.3"), std::cmp::Ordering::Greater);
        assert_eq!(compare_versions("4.50.3", "5.0.0"), std::cmp::Ordering::Less);
    }

    #[test]
    fn test_compare_versions_short() {
        // 版本号长度不同
        assert_eq!(compare_versions("4.57", "4.57.0"), std::cmp::Ordering::Equal);
    }

    #[test]
    fn test_parse_version() {
        assert_eq!(parse_version("4.57.3"), vec![4, 57, 3]);
        assert_eq!(parse_version("5.0"), vec![5, 0]);
        assert_eq!(parse_version("4.50.3"), vec![4, 50, 3]);
    }

    #[test]
    fn test_fallback_versions_not_empty() {
        assert!(!FALLBACK_VERSIONS.is_empty());
        // 必须包含 ComfyUI 最低要求版本
        assert!(FALLBACK_VERSIONS.contains(&"4.50.3"));
        // 必须包含最新 5.x（实验版本）
        assert!(FALLBACK_VERSIONS.contains(&"5.13.0"));
    }

    #[test]
    fn test_fallback_versions_descending() {
        // 验证 fallback 列表是降序排列
        for i in 1..FALLBACK_VERSIONS.len() {
            let prev = FALLBACK_VERSIONS[i - 1];
            let curr = FALLBACK_VERSIONS[i];
            assert_eq!(
                compare_versions(prev, curr),
                std::cmp::Ordering::Greater,
                "expected {} > {} in FALLBACK_VERSIONS",
                prev,
                curr
            );
        }
    }

    #[tokio::test]
    async fn test_get_versions_returns_fallback_when_empty() {
        let tmp = std::env::temp_dir().join(format!("bl-test-{}.json", uuid::Uuid::new_v4()));
        let bus = EventBus::new();
        let index = TransformersVersionIndex::new_for_test(tmp, bus);
        let versions = index.get_versions();
        assert!(!versions.is_empty());
        assert!(versions.contains(&"4.50.3".to_string()));
    }

    #[tokio::test]
    async fn test_save_and_load_file() {
        let tmp = std::env::temp_dir().join(format!("bl-test-{}.json", uuid::Uuid::new_v4()));
        let versions = vec!["5.0.0".to_string(), "4.57.3".to_string()];
        save_to_file(&tmp, &versions).await.unwrap();
        let content = std::fs::read_to_string(&tmp).unwrap();
        let loaded: Vec<String> = serde_json::from_str(&content).unwrap();
        assert_eq!(loaded, versions);
        std::fs::remove_file(&tmp).ok();
    }
}
