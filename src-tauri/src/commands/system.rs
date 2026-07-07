//! System Tauri commands（v3.0 新增）
//!
//! - `system_detect_gpus` — 跨平台 GPU 检测
//! - `system_clear_gpu_cache` — 清除 5 分钟缓存
//! - `system_recommend_torch` — 智能推荐 TorchVariant
//! - `system_check_driver_compat` — v3.10：驱动兼容性深度检查

use crate::python_env::TorchVariant;
use crate::system::{
    check_driver_compatibility_full, clear_gpu_cache, detect_and_cache, recommend_torch_variant,
    DriverCompatReport,
};

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

/// v3.10：驱动兼容性深度检查
///
/// 一站式入口：检测 GPU + 推荐变体 + 驱动兼容性检查
///
/// 返回 DriverCompatReport：
/// - `severity`：整体严重度（Ok / Warning / Error）
/// - `gpus`：检测到的所有 GPU
/// - `recommended_variant`：综合驱动兼容性后的最终推荐
/// - `notes`：每条兼容性诊断
/// - `recommendation`：用户可读的修复建议
///
/// 用例：前端「智能推荐」按钮调用，根据报告决定是否降级或切换到 CPU
#[tauri::command]
pub async fn system_check_driver_compat() -> Result<DriverCompatReport, String> {
    Ok(check_driver_compatibility_full().await)
}
