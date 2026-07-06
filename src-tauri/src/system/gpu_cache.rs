//! GPU 检测结果缓存（v3.0 新增，v3.6 改用 CancellationToken）
//!
//! 5 分钟 TTL，避免每次设置页打开都重新调外部工具。
//! 用 Mutex<Vec<GpuInfo>> + 时间戳实现（无锁全局状态用 OnceCell，可选）

use std::sync::OnceLock;
use std::time::Instant;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use super::gpu::{detect_gpus, GpuInfo};

const CACHE_TTL_SECS: u64 = 5 * 60;

struct CacheEntry {
    gpus: Vec<GpuInfo>,
    cached_at: Instant,
}

static CACHE: OnceLock<Mutex<Option<CacheEntry>>> = OnceLock::new();

fn cache_cell() -> &'static Mutex<Option<CacheEntry>> {
    CACHE.get_or_init(|| Mutex::new(None))
}

/// 拿缓存的 GPU 列表（命中且未过期）
pub fn get_cached_gpus() -> Option<Vec<GpuInfo>> {
    let cell = cache_cell().lock();
    let entry = cell.as_ref()?;
    if entry.cached_at.elapsed().as_secs() < CACHE_TTL_SECS {
        Some(entry.gpus.clone())
    } else {
        None
    }
}

/// 检测 GPU 并写入缓存
///
/// v3.6：detect_gpus 现在需要 CancellationToken，这里创建一个本地非可取消 token
/// （调用方不持有 token，意味着此检测不可被外部取消）。
pub async fn detect_and_cache() -> Vec<GpuInfo> {
    let cancel = CancellationToken::new();
    let gpus = detect_gpus(&cancel).await;
    *cache_cell().lock() = Some(CacheEntry {
        gpus: gpus.clone(),
        cached_at: Instant::now(),
    });
    gpus
}

/// 清除缓存
pub fn clear_gpu_cache() {
    *cache_cell().lock() = None;
}

/// 拿或重新检测（命中且未过期直接返回，否则重新检测）
pub async fn get_or_detect() -> Vec<GpuInfo> {
    if let Some(cached) = get_cached_gpus() {
        return cached;
    }
    detect_and_cache().await
}
