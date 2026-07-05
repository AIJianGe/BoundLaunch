//! GPU 自动检测（v3.0 新增）
//!
//! 跨平台调用厂商专用工具：
//! - NVIDIA: `nvidia-smi` (Windows / Linux)
//! - AMD: `rocm-smi` (Linux) / WMI (Windows)
//! - Intel: WMI / `sycl-ls` (Linux)
//! - Apple: `system_profiler` (macOS)
//!
//! 每个检测独立 5s 超时 + try_catch，任一失败不影响其他。
//! 总检测时间 < 5s（并行）。

use std::process::Command;
use std::time::Duration;

use serde::Serialize;
use tokio::time::timeout;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Apple,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct GpuInfo {
    pub vendor: GpuVendor,
    pub model: String,
    pub vram_mb: Option<u64>,
    pub driver_version: Option<String>,
    pub cuda_version: Option<String>,
    pub rocm_version: Option<String>,
}

/// 检测所有 GPU（并行 + 各自 5s 超时）
pub async fn detect_gpus() -> Vec<GpuInfo> {
    let (nvidia, amd, intel, apple) = tokio::join!(
        detect_nvidia(),
        detect_amd(),
        detect_intel(),
        detect_apple(),
    );
    let mut all = Vec::new();
    all.extend(nvidia);
    all.extend(amd);
    all.extend(intel);
    all.extend(apple);
    all
}

// ===== NVIDIA =====

async fn detect_nvidia() -> Vec<GpuInfo> {
    // nvidia-smi --query-gpu=name,memory.total,driver_version --format=csv,noheader,nounits
    let result = timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(|| {
            let output = Command::new("nvidia-smi")
                .args([
                    "--query-gpu=name,memory.total,driver_version",
                    "--format=csv,noheader,nounits",
                ])
                .output();

            match output {
                Ok(out) if out.status.success() => {
                    let text = String::from_utf8_lossy(&out.stdout);
                    let gpus: Vec<GpuInfo> = text
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .filter_map(parse_nvidia_line)
                        .collect();
                    // 额外尝试解析 CUDA 版本（从头部）
                    let cuda_version = extract_cuda_version();
                    Ok::<_, String>((gpus, cuda_version))
                }
                _ => Ok((Vec::new(), None)),
            }
        }),
    )
    .await;

    match result {
        Ok(Ok(Ok((mut gpus, cuda)))) => {
            // 附加 CUDA 版本到所有 NVIDIA 卡
            for g in &mut gpus {
                g.cuda_version = cuda.clone();
            }
            gpus
        }
        _ => Vec::new(),
    }
}

fn parse_nvidia_line(line: &str) -> Option<GpuInfo> {
    // 格式: "GeForce RTX 4080, 16376, 560.94"
    let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
    if parts.is_empty() {
        return None;
    }
    Some(GpuInfo {
        vendor: GpuVendor::Nvidia,
        model: parts.first().unwrap_or(&"Unknown NVIDIA GPU").to_string(),
        vram_mb: parts.get(1).and_then(|s| s.parse::<u64>().ok()),
        driver_version: parts.get(2).map(|s| s.to_string()),
        cuda_version: None, // 由调用方填充
        rocm_version: None,
    })
}

/// 从 `nvidia-smi` 头部提取 CUDA Version
fn extract_cuda_version() -> Option<String> {
    let output = Command::new("nvidia-smi").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if let Some(idx) = line.find("CUDA Version:") {
            let after = &line[idx + "CUDA Version:".len()..];
            return after
                .trim()
                .split_whitespace()
                .next()
                .map(|s| s.to_string());
        }
    }
    None
}

// ===== AMD =====

async fn detect_amd() -> Vec<GpuInfo> {
    // Linux: rocm-smi --showproductname --csv
    // Windows: PowerShell WMI
    #[cfg(target_os = "linux")]
    {
        detect_amd_linux().await
    }
    #[cfg(target_os = "windows")]
    {
        detect_amd_windows().await
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
async fn detect_amd_linux() -> Vec<GpuInfo> {
    let result = timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(|| {
            let output = Command::new("rocm-smi")
                .args(["--showproductname", "--csv"])
                .output();
            match output {
                Ok(out) if out.status.success() => {
                    let text = String::from_utf8_lossy(&out.stdout);
                    Ok::<_, String>(parse_amd_csv(&text))
                }
                _ => Ok(Vec::new()),
            }
        }),
    )
    .await;

    match result {
        Ok(Ok(Ok(gpus))) => gpus,
        _ => Vec::new(),
    }
}

fn parse_amd_csv(text: &str) -> Vec<GpuInfo> {
    // rocm-smi --csv 格式（简化）:
    // device,Product Name
    // 0,Radeon RX 7900 XT
    let mut gpus = Vec::new();
    let mut first = true;
    for line in text.lines() {
        if first {
            first = false;
            continue; // 跳过头部
        }
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() < 2 {
            continue;
        }
        let model = parts[1..].join(",").trim().to_string();
        if model.is_empty() {
            continue;
        }
        gpus.push(GpuInfo {
            vendor: GpuVendor::Amd,
            model,
            vram_mb: None,
            driver_version: None,
            cuda_version: None,
            rocm_version: None,
        });
    }
    gpus
}

#[cfg(target_os = "windows")]
async fn detect_amd_windows() -> Vec<GpuInfo> {
    let result = timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(|| {
            // PowerShell: Get-CimInstance Win32_VideoController | Where Name -match "AMD|Radeon"
            let output = Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-Command",
                    "Get-CimInstance Win32_VideoController | Where-Object { $_.Name -match 'AMD|Radeon' } | Select-Object -ExpandProperty Name",
                ])
                .output();
            match output {
                Ok(out) if out.status.success() => {
                    let text = String::from_utf8_lossy(&out.stdout);
                    Ok::<_, String>(
                        text.lines()
                            .filter(|l| !l.trim().is_empty())
                            .map(|name| GpuInfo {
                                vendor: GpuVendor::Amd,
                                model: name.trim().to_string(),
                                vram_mb: None,
                                driver_version: None,
                                cuda_version: None,
                                rocm_version: None,
                            })
                            .collect(),
                    )
                }
                _ => Ok(Vec::new()),
            }
        }),
    )
    .await;

    match result {
        Ok(Ok(Ok(gpus))) => gpus,
        _ => Vec::new(),
    }
}

// ===== Intel =====

async fn detect_intel() -> Vec<GpuInfo> {
    #[cfg(target_os = "windows")]
    {
        detect_intel_windows().await
    }
    #[cfg(not(target_os = "windows"))]
    {
        detect_intel_unix().await
    }
}

#[cfg(target_os = "windows")]
async fn detect_intel_windows() -> Vec<GpuInfo> {
    let result = timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(|| {
            let output = Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-Command",
                    "Get-CimInstance Win32_VideoController | Where-Object { $_.Name -match 'Intel.*Arc|Intel.*Graphics' -and $_.Name -notmatch 'UHD|Iris' } | Select-Object -ExpandProperty Name",
                ])
                .output();
            match output {
                Ok(out) if out.status.success() => {
                    let text = String::from_utf8_lossy(&out.stdout);
                    Ok::<_, String>(
                        text.lines()
                            .filter(|l| !l.trim().is_empty())
                            .map(|name| GpuInfo {
                                vendor: GpuVendor::Intel,
                                model: name.trim().to_string(),
                                vram_mb: None,
                                driver_version: None,
                                cuda_version: None,
                                rocm_version: None,
                            })
                            .collect(),
                    )
                }
                _ => Ok(Vec::new()),
            }
        }),
    )
    .await;

    match result {
        Ok(Ok(Ok(gpus))) => gpus,
        _ => Vec::new(),
    }
}

#[cfg(not(target_os = "windows"))]
async fn detect_intel_unix() -> Vec<GpuInfo> {
    // Linux: sycl-ls 2>/dev/null | grep "Intel"
    let result = timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(|| {
            let output = Command::new("sycl-ls").output();
            match output {
                Ok(out) if out.status.success() => {
                    let text = String::from_utf8_lossy(&out.stdout);
                    let gpus: Vec<GpuInfo> = text
                        .lines()
                        .filter(|l| l.contains("Intel"))
                        .map(|line| GpuInfo {
                            vendor: GpuVendor::Intel,
                            model: line.trim().to_string(),
                            vram_mb: None,
                            driver_version: None,
                            cuda_version: None,
                            rocm_version: None,
                        })
                        .collect();
                    Ok::<_, String>(gpus)
                }
                _ => Ok(Vec::new()),
            }
        }),
    )
    .await;

    match result {
        Ok(Ok(Ok(gpus))) => gpus,
        _ => Vec::new(),
    }
}

// ===== Apple =====

#[cfg(target_os = "macos")]
async fn detect_apple() -> Vec<GpuInfo> {
    let result = timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(|| {
            let output = Command::new("system_profiler")
                .args(["SPDisplaysDataType", "-json"])
                .output();
            match output {
                Ok(out) if out.status.success() => {
                    let text = String::from_utf8_lossy(&out.stdout);
                    Ok::<_, String>(parse_apple_json(&text))
                }
                _ => Ok(Vec::new()),
            }
        }),
    )
    .await;

    match result {
        Ok(Ok(Ok(gpus))) => gpus,
        _ => Vec::new(),
    }
}

#[cfg(target_os = "macos")]
fn parse_apple_json(text: &str) -> Vec<GpuInfo> {
    let json: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let displays = json.get("SPDisplaysDataType").and_then(|v| v.as_array());
    let Some(displays) = displays else {
        return Vec::new();
    };
    displays
        .iter()
        .filter_map(|d| {
            let model = d.get("spdisplays_device_name")?.as_str()?.to_string();
            Some(GpuInfo {
                vendor: GpuVendor::Apple,
                model,
                vram_mb: d.get("spdisplays_vram").and_then(|v| v.as_str()).and_then(|s| {
                    // "8 GB" → 8192
                    let num: u64 = s.split_whitespace().next()?.parse().ok()?;
                    Some(num * 1024)
                }),
                driver_version: None,
                cuda_version: None,
                rocm_version: None,
            })
        })
        .collect()
}

#[cfg(not(target_os = "macos"))]
async fn detect_apple() -> Vec<GpuInfo> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_nvidia_line() {
        let line = "GeForce RTX 4080, 16376, 560.94";
        let info = parse_nvidia_line(line).unwrap();
        assert_eq!(info.vendor, GpuVendor::Nvidia);
        assert_eq!(info.model, "GeForce RTX 4080");
        assert_eq!(info.vram_mb, Some(16376));
        assert_eq!(info.driver_version, Some("560.94".to_string()));
    }

    #[test]
    fn test_parse_amd_csv() {
        let text = "device,Product Name\n0,Radeon RX 7900 XT\n1,Radeon RX 6800";
        let gpus = parse_amd_csv(text);
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].vendor, GpuVendor::Amd);
        assert_eq!(gpus[0].model, "Radeon RX 7900 XT");
        assert_eq!(gpus[1].model, "Radeon RX 6800");
    }

    #[test]
    fn test_parse_amd_csv_empty() {
        let gpus = parse_amd_csv("");
        assert!(gpus.is_empty());
    }
}
