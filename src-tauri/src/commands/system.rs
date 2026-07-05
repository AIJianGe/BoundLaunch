//! System Tauri commands（v3.0 新增）
//!
//! - `system_detect_gpus` — 跨平台 GPU 检测
//! - `system_clear_gpu_cache` — 清除 5 分钟缓存
//! - `system_recommend_torch` — 智能推荐 TorchVariant

use crate::python_env::TorchVariant;
use crate::system::{clear_gpu_cache, detect_and_cache, recommend_torch_variant};

/// 跨平台 GPU 检测（带 5 分钟缓存）
#[tauri::command]
pub async fn system_detect_gpus(force_refresh: bool) -> Result<Vec<crate::system::GpuInfo>, String> {
    if force_refresh {
        clear_gpu_cache();
    }
    let gpus = if force_refresh {
        detect_and_cache().await
    } else {
        // 尝试拿缓存，命中失败再检测
        match crate::system::get_cached_gpus() {
            Some(c) => c,
            None => detect_and_cache().await,
        }
    };
    Ok(gpus)
}

/// 清除 GPU 检测缓存
#[tauri::command]
pub fn system_clear_gpu_cache() -> Result<(), String> {
    clear_gpu_cache();
    Ok(())
}

/// 智能推荐 TorchVariant
#[tauri::command]
pub async fn system_recommend_torch() -> Result<TorchVariant, String> {
    Ok(recommend_torch_variant().await)
}
