//! **v3.x Phase 3**：硬件指纹 + 变化检测
//!
//! ## 背景
//!
//! 用户可能因为以下原因导致硬件变化：
//! - 升级了 NVIDIA 驱动
//! - 更换了显卡（RTX 4080 → RTX 5090）
//! - 把已经装好环境的目录**复制到别的机器**解压
//!
//! 硬件变化会导致**已装的 PyTorch 与新硬件不兼容**：
//! - 旧 torch 装的是 CUDA 12.6，驱动升到 13.0 后某些算子失效
//! - 旧 torch 装的是 RTX 4080 优化的 kernel，复制到 RTX 5090 的机器上跑慢
//! - 跨机器复制后 venv 里的 torch 完全失效
//!
//! ## 解决方案
//!
//! 1. **每次启动**探测硬件 → 计算 fingerprint（hash of GPU型号列表 + NVIDIA 驱动）
//! 2. **SQLite 存**上次 fingerprint
//! 3. **对比**新 vs 旧 → 不一致时产生 `HardwareChangeReport`
//! 4. 前端根据 `report.recommended_action` 决定是否弹窗
//!
//! ## 跨机器场景
//!
//! 第一次在新机器启动时**没**历史 fingerprint，视为"首次在新机器" → 不弹窗（避免误报）
//! 第二次启动时如果硬件有变 → 弹窗

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::gpu::{detect_gpus, GpuInfo};

/// 硬件指纹（一次探测的快照）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HardwareFingerprint {
    /// GPU 型号列表（如 ["GeForce RTX 4090", "GeForce RTX 5090"]）
    pub gpu_models: Vec<String>,
    /// 驱动版本（如 Some("560.94") 或 None）
    pub nvidia_driver: Option<String>,
    /// **fingerprint hash**（gpu_models + nvidia_driver 的 hash）
    pub fingerprint_hash: String,
    /// 探测时间
    pub recorded_at: DateTime<Utc>,
}

/// 推荐的应对动作（前端弹窗决策依据）
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecommendedAction {
    /// 无变化，不需要任何操作
    NoAction,
    /// 检测到硬件变化，**强烈建议**重装 torch
    ReinstallTorch,
    /// 变化但**不强制**（如驱动小版本升级）
    Optional,
}

/// 硬件变化报告
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareChangeReport {
    /// 整体是否变化
    pub has_change: bool,
    /// 当前硬件指纹
    pub current: HardwareFingerprint,
    /// 上次记录的硬件指纹（None = 首次记录）
    pub previous: Option<HardwareFingerprint>,
    /// 推荐动作
    pub recommended_action: RecommendedAction,
    /// 用户可读的诊断信息
    pub notes: Vec<String>,
}

/// 计算 fingerprint hash
fn compute_fingerprint_hash(gpu_models: &[String], nvidia_driver: &Option<String>) -> String {
    let mut hasher = DefaultHasher::new();
    for model in gpu_models {
        model.hash(&mut hasher);
    }
    if let Some(driver) = nvidia_driver {
        driver.hash(&mut hasher);
    }
    format!("{:x}", hasher.finish())
}

/// 探测当前硬件并生成 fingerprint
pub async fn detect_fingerprint() -> HardwareFingerprint {
    // 使用 `get_or_detect` 走 5 分钟缓存（避免每次启动都重新执行 nvidia-smi / wmic）
    let gpus = super::gpu_cache::get_or_detect().await;
    let gpu_models: Vec<String> = gpus.iter().map(|g| g.model.clone()).collect();
    let nvidia_driver = gpus
        .iter()
        .find(|g| matches!(g.vendor, super::gpu::GpuVendor::Nvidia))
        .and_then(|g| g.driver_version.clone());
    let fingerprint_hash = compute_fingerprint_hash(&gpu_models, &nvidia_driver);
    HardwareFingerprint {
        gpu_models,
        nvidia_driver,
        fingerprint_hash,
        recorded_at: Utc::now(),
    }
}

/// 从 SQLite 读上次 fingerprint
pub async fn get_stored_fingerprint(pool: &SqlitePool) -> Result<Option<HardwareFingerprint>, sqlx::Error> {
    let row: Option<(String, Option<String>, String, String)> = sqlx::query_as(
        "SELECT gpu_models, nvidia_driver, fingerprint_hash, recorded_at
         FROM hardware_fingerprint
         ORDER BY id DESC
         LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;

    if let Some((gpu_models_json, nvidia_driver, fingerprint_hash, recorded_at)) = row {
        let gpu_models: Vec<String> = serde_json::from_str(&gpu_models_json).unwrap_or_default();
        let recorded_at = DateTime::parse_from_rfc3339(&recorded_at)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        Ok(Some(HardwareFingerprint {
            gpu_models,
            nvidia_driver,
            fingerprint_hash,
            recorded_at,
        }))
    } else {
        Ok(None)
    }
}

/// 写 fingerprint 到 SQLite（覆盖式）
pub async fn store_fingerprint(pool: &SqlitePool, fp: &HardwareFingerprint) -> Result<(), sqlx::Error> {
    let gpu_models_json = serde_json::to_string(&fp.gpu_models).unwrap_or_else(|_| "[]".to_string());

    // 启动时建表（如果不存在）
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS hardware_fingerprint (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            gpu_models      TEXT    NOT NULL,
            nvidia_driver   TEXT,
            fingerprint_hash TEXT   NOT NULL,
            recorded_at     TEXT    NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    // 删旧记录，插新记录
    sqlx::query("DELETE FROM hardware_fingerprint")
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO hardware_fingerprint (gpu_models, nvidia_driver, fingerprint_hash, recorded_at)
         VALUES (?, ?, ?, ?)",
    )
    .bind(&gpu_models_json)
    .bind(&fp.nvidia_driver)
    .bind(&fp.fingerprint_hash)
    .bind(fp.recorded_at.to_rfc3339())
    .execute(pool)
    .await?;

    tracing::info!(
        hash = %fp.fingerprint_hash,
        gpu_count = fp.gpu_models.len(),
        driver = ?fp.nvidia_driver,
        "v3.x Phase 3: 硬件指纹已存储"
    );
    Ok(())
}

/// **核心入口**：检测当前硬件 + 读历史 → 生成变化报告
///
/// 设计：
/// - 探测失败（无 GPU / nvidia-smi 不可用）→ 视为"首次记录"，不弹窗
/// - 历史为空 → 视为"首次记录"，不弹窗
/// - hash 相同 → 无变化，不弹窗
/// - hash 不同：
///   - GPU 型号不同 → 强烈建议重装（ReinstallTorch）
///   - 只是驱动版本不同 → 可选（Optional）
pub async fn check_hardware_change(pool: &SqlitePool) -> HardwareChangeReport {
    let current = detect_fingerprint().await;
    let previous = match get_stored_fingerprint(pool).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "v3.x Phase 3: 读取历史 fingerprint 失败");
            None
        }
    };

    // 探测到 0 块 GPU + 历史有 GPU → 异常（驱动可能卸载了）
    // 不弹窗，但记录 warning
    if current.gpu_models.is_empty() && previous.is_some() {
        tracing::warn!("v3.x Phase 3: 当前未检测到 GPU，但历史有 GPU 记录");
    }

    // 首次记录（无历史）→ 写入 + 返回 NoAction
    if previous.is_none() {
        let _ = store_fingerprint(pool, &current).await;
        return HardwareChangeReport {
            has_change: false,
            current,
            previous: None,
            recommended_action: RecommendedAction::NoAction,
            notes: vec!["首次记录硬件指纹".to_string()],
        };
    }

    let previous = previous.unwrap();
    let has_change = current.fingerprint_hash != previous.fingerprint_hash;

    if !has_change {
        // 无变化，更新 recorded_at（保持新鲜）
        let _ = store_fingerprint(pool, &current).await;
        return HardwareChangeReport {
            has_change: false,
            current,
            previous: Some(previous),
            recommended_action: RecommendedAction::NoAction,
            notes: vec!["硬件未变化".to_string()],
        };
    }

    // 有变化 → 分析
    let mut notes = Vec::new();
    let mut action = RecommendedAction::ReinstallTorch;

    // 比较 GPU 型号
    let prev_models: std::collections::HashSet<&String> = previous.gpu_models.iter().collect();
    let curr_models: std::collections::HashSet<&String> = current.gpu_models.iter().collect();
    let added: Vec<&&String> = curr_models.difference(&prev_models).collect();
    let removed: Vec<&&String> = prev_models.difference(&curr_models).collect();

    if !added.is_empty() || !removed.is_empty() {
        notes.push(format!(
            "GPU 列表变化: +{:?} -{:?}",
            added.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            removed.iter().map(|s| s.as_str()).collect::<Vec<_>>()
        ));
        action = RecommendedAction::ReinstallTorch;
    }

    // 比较驱动版本
    if previous.nvidia_driver != current.nvidia_driver {
        notes.push(format!(
            "NVIDIA 驱动变化: {} → {}",
            previous.nvidia_driver.as_deref().unwrap_or("(无)"),
            current.nvidia_driver.as_deref().unwrap_or("(无)")
        ));
        // 驱动变化时**降低**推荐强度（驱动小版本升级不需要重装 torch）
        if added.is_empty() && removed.is_empty() {
            action = RecommendedAction::Optional;
        }
    }

    notes.push("建议重新安装 torch 以匹配新硬件".to_string());

    // 写新 fingerprint
    let _ = store_fingerprint(pool, &current).await;

    HardwareChangeReport {
        has_change: true,
        current,
        previous: Some(previous),
        recommended_action: action,
        notes,
    }
}

/// 探测 venv 里的 torch 与 cfg.torch.cuda_version 是否一致
///
/// 通过 `python -c "import torch; print(torch.version.cuda, torch.cuda.is_available())"`
/// 拿 venv 里 torch 的实际 CUDA 版本和 CUDA 可用性。
///
/// 返回 Some(()) 表示一致，Some(Err(msg)) 表示不一致，None 表示探测失败（无 torch 等）。
pub async fn check_venv_torch_consistency(
    venv_python: &std::path::Path,
    configured_cuda: &str,
) -> Option<Result<(), String>> {
    use tokio::process::Command;

    let python = if venv_python.exists() {
        venv_python.to_path_buf()
    } else {
        return None;
    };

    let output = Command::new(&python)
        .arg("-c")
        .arg("import torch; print(torch.version.cuda or 'cpu', torch.cuda.is_available())")
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.trim();
    let mut parts = line.split_whitespace();
    let actual_cuda = parts.next().unwrap_or("");
    let cuda_available: bool = parts.next().and_then(|s| s.parse().ok()).unwrap_or(false);

    if !cuda_available {
        return Some(Err("venv 里 torch 不支持 CUDA".to_string()));
    }
    if actual_cuda != configured_cuda {
        return Some(Err(format!(
            "venv 里 torch 装的是 CUDA {}，但配置是 CUDA {}",
            actual_cuda, configured_cuda
        )));
    }
    Some(Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_fingerprint_hash_stable() {
        let h1 = compute_fingerprint_hash(&["RTX 4090".to_string()], &Some("560.94".to_string()));
        let h2 = compute_fingerprint_hash(&["RTX 4090".to_string()], &Some("560.94".to_string()));
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_compute_fingerprint_hash_driver_change() {
        let h1 = compute_fingerprint_hash(&["RTX 4090".to_string()], &Some("560.94".to_string()));
        let h2 = compute_fingerprint_hash(&["RTX 4090".to_string()], &Some("560.95".to_string()));
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_compute_fingerprint_hash_gpu_change() {
        let h1 = compute_fingerprint_hash(&["RTX 4080".to_string()], &Some("560.94".to_string()));
        let h2 = compute_fingerprint_hash(&["RTX 4090".to_string()], &Some("560.94".to_string()));
        assert_ne!(h1, h2);
    }
}
