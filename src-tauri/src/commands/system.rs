//! System Tauri commands（v3.0 新增）
//!
//! - `system_detect_gpus` — 跨平台 GPU 检测
//! - `system_clear_gpu_cache` — 清除 5 分钟缓存
//! - `system_recommend_torch` — 智能推荐 TorchVariant
//! - `system_check_driver_compat` — v3.10：驱动兼容性深度检查

use crate::python_env::TorchVariant;
use crate::system::{
    check_driver_compatibility_full, check_hardware_change, clear_gpu_cache, detect_and_cache,
    recommend_torch_variant, DriverCompatReport, HardwareChangeReport,
};
use crate::app_state::AppState;

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

/// **v3.x Phase 3**：硬件变化检测
///
/// 探测当前硬件 + 读 SQLite 历史 → 返回变化报告。
///
/// 行为：
/// - 首次记录（无历史）→ 写入 + 返回 `has_change: false`
/// - 无变化 → 返回 `has_change: false`
/// - 有变化（GPU 列表不同 或 NVIDIA 驱动不同）→ 返回 `has_change: true` + 推荐动作
///
/// 前端应在启动 5-10 秒后调用，根据 `recommended_action` 决定是否弹窗。
#[tauri::command]
pub async fn system_check_hardware_change(
    state: tauri::State<'_, AppState>,
) -> Result<HardwareChangeReport, String> {
    let pool = state.log_store.pool();
    Ok(check_hardware_change(pool).await)
}

/// **v3.x Phase 6**：venv 里 torch 一致性检测
///
/// 用 `python -c "import torch; print(torch.version.cuda, torch.cuda.is_available())"`
/// 拿 venv 里 torch 的实际 CUDA 版本，对比配置。
///
/// 返回：
/// - `None` → 探测失败（无 python / 无 torch / python 启动失败）
/// - `Some(Ok(()))` → 一致
/// - `Some(Err(msg))` → 不一致，msg 是诊断信息
#[tauri::command]
pub async fn system_check_venv_torch_consistency(
    venv_python: String,
    configured_cuda: String,
) -> Result<Option<Result<(), String>>, String> {
    Ok(
        crate::system::check_venv_torch_consistency(
            std::path::Path::new(&venv_python),
            &configured_cuda,
        )
        .await,
    )
}
