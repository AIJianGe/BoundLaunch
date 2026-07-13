//! 下载模块：异步下载 zip + 解压到 staging
//!
//! ## 流程
//!
//! 1. 下载到临时文件（`<staging>/download.zip.tmp`）
//! 2. 进度事件：`update_progress { phase: "download", percent, speed_bps, eta_seconds }`
//! 3. 下载完成 → 校验 SHA256（可选）
//! 4. 解压 zip → `<staging>/` 目录
//! 5. 删除临时 zip
//!
//! ## 错误恢复
//!
//! - 下载失败：保留 staging 目录，用户可重试
//! - SHA256 不匹配：清掉 staging 目录，返回错误
//! - 解压失败：清掉 staging 目录，返回错误
//!
//! ## 取消
//!
//! 调用方传 `CancellationToken`，定期检查。

use std::path::Path;
use std::time::Instant;

use futures_util::StreamExt;
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

use super::manifest::UpdateInfo;
use super::paths;
use crate::error::ProcessError;

#[derive(Debug, Clone, serde::Serialize)]
pub struct UpdateProgress {
    /// 阶段：download | verify | extract
    pub phase: ProgressPhase,
    /// 进度百分比（0-100）
    pub percent: f32,
    /// 已处理字节数
    pub bytes_done: u64,
    /// 总字节数（未知时为 0）
    pub bytes_total: u64,
    /// 当前速度（字节/秒，0 表示无速度样本）
    pub speed_bps: u64,
    /// 预计剩余秒数（0 表示无法估算）
    pub eta_seconds: u64,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressPhase {
    Download,
    Verify,
    Extract,
}

/// 进度事件名
pub const EVENT_UPDATE_PROGRESS: &str = "update_progress";

/// 主动下载并解压更新
///
/// 返回：解压后的 staging 根目录路径
pub async fn download_and_extract(
    app: &AppHandle,
    info: &UpdateInfo,
    cancel: CancellationToken,
) -> Result<std::path::PathBuf, ProcessError> {
    let staging = paths::staging_dir(&info.latest_version);
    let tmp_zip = staging.join("download.zip.tmp");

    // 1) 准备 staging 目录
    if staging.exists() {
        // 残留旧 staging → 清理
        std::fs::remove_dir_all(&staging)
            .map_err(|e| ProcessError::Other(format!("清理 staging 失败: {}", e)))?;
    }
    std::fs::create_dir_all(&staging)
        .map_err(|e| ProcessError::Other(format!("创建 staging 失败: {}", e)))?;

    // 2) 下载 zip
    download_zip(app, &info.download_url, info.zip_size, &tmp_zip, cancel.clone()).await?;

    // 3) SHA256 校验（可选）
    if let Some(sha256_url) = &info.sha256 {
        emit_progress(
            app,
            UpdateProgress {
                phase: ProgressPhase::Verify,
                percent: 0.0,
                bytes_done: 0,
                bytes_total: tmp_zip.metadata().map(|m| m.len()).unwrap_or(0),
                speed_bps: 0,
                eta_seconds: 0,
            },
        );
        let expected = fetch_sha256(app, sha256_url, cancel.clone()).await?;
        let actual = compute_sha256(&tmp_zip).await?;
        if !expected.eq_ignore_ascii_case(&actual) {
            // 校验失败 → 清理
            let _ = std::fs::remove_dir_all(&staging);
            return Err(ProcessError::Other(format!(
                "SHA256 校验失败：\n预期: {}\n实际: {}",
                expected, actual
            )));
        }
        tracing::info!("SHA256 校验通过");
    } else {
        tracing::warn!("未提供 SHA256 校验文件，跳过校验");
    }

    // 4) 解压 zip
    emit_progress(
        app,
        UpdateProgress {
            phase: ProgressPhase::Extract,
            percent: 0.0,
            bytes_done: 0,
            bytes_total: tmp_zip.metadata().map(|m| m.len()).unwrap_or(0),
            speed_bps: 0,
            eta_seconds: 0,
        },
    );
    extract_zip(&tmp_zip, &staging, app, cancel.clone()).await?;
    tracing::info!(staging = %staging.display(), "更新包解压完成");

    // 5) 删除 zip 临时文件（保留解压后的目录）
    let _ = std::fs::remove_file(&tmp_zip);

    Ok(staging)
}

/// 下载 zip 到本地（带进度 + 取消）
async fn download_zip(
    app: &AppHandle,
    url: &str,
    expected_size: u64,
    dest: &Path,
    cancel: CancellationToken,
) -> Result<(), ProcessError> {
    let client = reqwest::Client::builder()
        .user_agent("BoundLaunch-Updater/0.0.1")
        .timeout(std::time::Duration::from_secs(600)) // 10 分钟
        .build()
        .map_err(|e| ProcessError::Other(format!("构建 HTTP 客户端失败: {}", e)))?;

    tracing::info!(url = %url, expected_size, "开始下载更新包");
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| ProcessError::Other(format!("HTTP 请求失败: {}", e)))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(ProcessError::Other(format!(
            "下载失败：HTTP {}",
            status
        )));
    }

    let total = resp.content_length().unwrap_or(expected_size);
    let mut stream = resp.bytes_stream();
    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| ProcessError::Other(format!("创建下载文件失败: {}", e)))?;

    let mut downloaded: u64 = 0;
    let start = Instant::now();
    let mut last_emit = Instant::now();

    while let Some(chunk_result) = stream.next().await {
        if cancel.is_cancelled() {
            return Err(ProcessError::Other("下载已取消".to_string()));
        }
        let chunk = chunk_result
            .map_err(|e| ProcessError::Other(format!("下载流错误: {}", e)))?;
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .map_err(|e| ProcessError::Other(format!("写入文件失败: {}", e)))?;
        downloaded += chunk.len() as u64;

        // 节流：每 200ms emit 一次进度
        if last_emit.elapsed() >= std::time::Duration::from_millis(200) {
            let elapsed = start.elapsed().as_secs_f64();
            let speed_bps = if elapsed > 0.0 {
                (downloaded as f64 / elapsed) as u64
            } else {
                0
            };
            let eta_seconds = if speed_bps > 0 && total > downloaded {
                (total - downloaded) / speed_bps
            } else {
                0
            };
            let percent = if total > 0 {
                (downloaded as f64 / total as f64 * 100.0) as f32
            } else {
                0.0
            };
            emit_progress(
                app,
                UpdateProgress {
                    phase: ProgressPhase::Download,
                    percent,
                    bytes_done: downloaded,
                    bytes_total: total,
                    speed_bps,
                    eta_seconds,
                },
            );
            last_emit = Instant::now();
        }
    }

    // 100% 收尾
    let elapsed = start.elapsed().as_secs_f64();
    let avg_speed = if elapsed > 0.0 {
        (downloaded as f64 / elapsed) as u64
    } else {
        0
    };
    emit_progress(
        app,
        UpdateProgress {
            phase: ProgressPhase::Download,
            percent: 100.0,
            bytes_done: downloaded,
            bytes_total: total,
            speed_bps: avg_speed,
            eta_seconds: 0,
        },
    );

    tracing::info!(downloaded, elapsed = ?start.elapsed(), "下载完成");
    Ok(())
}

fn emit_progress(app: &AppHandle, progress: UpdateProgress) {
    if let Err(e) = app.emit(EVENT_UPDATE_PROGRESS, &progress) {
        tracing::warn!(error = %e, "emit update_progress failed");
    }
}

/// 拉取 SHA256 文本
///
/// **为什么 app 没用**：SHA256 文件很小（< 100 字节），没有"进度"概念，
/// 不需要 emit 进度事件
async fn fetch_sha256(
    _app: &AppHandle,
    url: &str,
    cancel: CancellationToken,
) -> Result<String, ProcessError> {
    let client = reqwest::Client::builder()
        .user_agent("BoundLaunch-Updater/0.0.1")
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| ProcessError::Other(format!("构建 HTTP 客户端失败: {}", e)))?;

    if cancel.is_cancelled() {
        return Err(ProcessError::Other("已取消".to_string()));
    }

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| ProcessError::Other(format!("拉取 SHA256 失败: {}", e)))?;

    let text = resp
        .text()
        .await
        .map_err(|e| ProcessError::Other(format!("读取 SHA256 失败: {}", e)))?;

    // SHA256 文件格式：`<hex>  filename\n` 或只有 hex
    let trimmed = text.trim();
    let hex = trimmed.split_whitespace().next().unwrap_or("").to_lowercase();
    if hex.len() != 64 {
        return Err(ProcessError::Other(format!(
            "SHA256 文件格式异常：{}",
            trimmed
        )));
    }
    Ok(hex)
}

/// 计算文件 SHA256
async fn compute_sha256(path: &Path) -> Result<String, ProcessError> {
    use sha2::{Digest, Sha256};
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| ProcessError::Other(format!("读取文件失败: {}", e)))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let hash = hasher.finalize();
    Ok(format!("{:x}", hash))
}

/// 解压 zip 到目标目录
async fn extract_zip(
    zip_path: &Path,
    dest_dir: &Path,
    app: &AppHandle,
    cancel: CancellationToken,
) -> Result<(), ProcessError> {
    let file = std::fs::File::open(zip_path)
        .map_err(|e| ProcessError::Other(format!("打开 zip 失败: {}", e)))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| ProcessError::Other(format!("读取 zip 失败: {}", e)))?;

    let total = archive.len();
    for i in 0..total {
        if cancel.is_cancelled() {
            return Err(ProcessError::Other("解压已取消".to_string()));
        }
        let mut entry = archive
            .by_index(i)
            .map_err(|e| ProcessError::Other(format!("读取 zip entry 失败: {}", e)))?;

        // 路径处理：去掉最外层目录（zip 通常包含 `BoundLaunch-portable-vX.Y.Z/` 前缀）
        let raw_path = match entry.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => {
                tracing::warn!(entry_name = entry.name(), "跳过异常路径");
                continue;
            }
        };
        // 取第一段作为外层目录 → 去掉
        let stripped = raw_path
            .components()
            .skip(1) // 跳过最外层目录
            .collect::<std::path::PathBuf>();

        // 如果是空（说明只有顶层目录），跳过
        if stripped.as_os_str().is_empty() {
            continue;
        }

        let out_path = dest_dir.join(&stripped);

        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)
                .map_err(|e| ProcessError::Other(format!("创建目录失败: {}", e)))?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| ProcessError::Other(format!("创建父目录失败: {}", e)))?;
            }
            let mut out_file = std::fs::File::create(&out_path)
                .map_err(|e| ProcessError::Other(format!("创建文件失败: {}", e)))?;
            std::io::copy(&mut entry, &mut out_file)
                .map_err(|e| ProcessError::Other(format!("写入文件失败: {}", e)))?;
        }

        // 进度
        let percent = (i + 1) as f32 / total as f32 * 100.0;
        emit_progress(
            app,
            UpdateProgress {
                phase: ProgressPhase::Extract,
                percent,
                bytes_done: (i + 1) as u64,
                bytes_total: total as u64,
                speed_bps: 0,
                eta_seconds: 0,
            },
        );
    }

    Ok(())
}
