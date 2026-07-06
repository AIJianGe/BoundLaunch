//! GPU 自动检测（v3.0 新增，v3.6 改用 CancellationToken）
//!
//! 跨平台调用厂商专用工具：
//! - NVIDIA: `nvidia-smi` (Windows / Linux)
//! - AMD: `rocm-smi` (Linux) / WMI (Windows)
//! - Intel: WMI / `sycl-ls` (Linux)
//! - Apple: `system_profiler` (macOS)
//!
//! v3.6：每个检测独立 CancellationToken，取消时显式 kill 子进程。
//! 任一失败不影响其他，并行执行。

use std::process::Stdio;

use serde::Serialize;
use tokio_util::sync::CancellationToken;

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

/// 检测所有 GPU（并行，v3.6 改用 CancellationToken）
pub async fn detect_gpus(cancel: &CancellationToken) -> Vec<GpuInfo> {
    let (nvidia, amd, intel, apple) = tokio::join!(
        detect_nvidia(cancel),
        detect_amd(cancel),
        detect_intel(cancel),
        detect_apple(cancel),
    );
    let mut all = Vec::new();
    all.extend(nvidia);
    all.extend(amd);
    all.extend(intel);
    all.extend(apple);
    all
}

// ===== NVIDIA =====

async fn detect_nvidia(cancel: &CancellationToken) -> Vec<GpuInfo> {
    // nvidia-smi --query-gpu=name,memory.total,driver_version --format=csv,noheader,nounits
    let mut cmd = crate::common::process_util::new_command("nvidia-smi");
    cmd.args([
            "--query-gpu=name,memory.total,driver_version",
            "--format=csv,noheader,nounits",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    match crate::common::subprocess::run_with_cancel(&mut cmd, cancel).await {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            let mut gpus: Vec<GpuInfo> = text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(parse_nvidia_line)
                .collect();
            // 额外尝试解析 CUDA 版本（从头部）
            let cuda_version = extract_cuda_version(cancel).await;
            // 附加 CUDA 版本到所有 NVIDIA 卡
            for g in &mut gpus {
                g.cuda_version = cuda_version.clone();
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

/// 从 `nvidia-smi` 头部提取 CUDA Version（v3.6 改用 CancellationToken）
async fn extract_cuda_version(cancel: &CancellationToken) -> Option<String> {
    let mut cmd = crate::common::process_util::new_command("nvidia-smi");
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let output = crate::common::subprocess::run_with_cancel(&mut cmd, cancel)
        .await
        .ok()?;
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

async fn detect_amd(cancel: &CancellationToken) -> Vec<GpuInfo> {
    // Linux: rocm-smi --showproductname --csv
    // Windows: PowerShell WMI
    #[cfg(target_os = "linux")]
    {
        detect_amd_linux(cancel).await
    }
    #[cfg(target_os = "windows")]
    {
        detect_amd_windows(cancel).await
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = cancel;
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
async fn detect_amd_linux(cancel: &CancellationToken) -> Vec<GpuInfo> {
    let mut cmd = crate::common::process_util::new_command("rocm-smi");
    cmd.args(["--showproductname", "--csv"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    match crate::common::subprocess::run_with_cancel(&mut cmd, cancel).await {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            parse_amd_csv(&text)
        }
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
async fn detect_amd_windows(cancel: &CancellationToken) -> Vec<GpuInfo> {
    // PowerShell: Get-CimInstance Win32_VideoController | Where Name -match "AMD|Radeon"
    let mut cmd = crate::common::process_util::new_command("powershell");
    cmd.args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_VideoController | Where-Object { $_.Name -match 'AMD|Radeon' } | Select-Object -ExpandProperty Name",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    match crate::common::subprocess::run_with_cancel(&mut cmd, cancel).await {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
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
                .collect()
        }
        _ => Vec::new(),
    }
}

// ===== Intel =====

async fn detect_intel(cancel: &CancellationToken) -> Vec<GpuInfo> {
    #[cfg(target_os = "windows")]
    {
        detect_intel_windows(cancel).await
    }
    #[cfg(not(target_os = "windows"))]
    {
        detect_intel_unix(cancel).await
    }
}

#[cfg(target_os = "windows")]
async fn detect_intel_windows(cancel: &CancellationToken) -> Vec<GpuInfo> {
    let mut cmd = crate::common::process_util::new_command("powershell");
    cmd.args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_VideoController | Where-Object { $_.Name -match 'Intel.*Arc|Intel.*Graphics' -and $_.Name -notmatch 'UHD|Iris' } | Select-Object -ExpandProperty Name",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    match crate::common::subprocess::run_with_cancel(&mut cmd, cancel).await {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
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
                .collect()
        }
        _ => Vec::new(),
    }
}

#[cfg(not(target_os = "windows"))]
async fn detect_intel_unix(cancel: &CancellationToken) -> Vec<GpuInfo> {
    // Linux: sycl-ls 2>/dev/null | grep "Intel"
    let mut cmd = crate::common::process_util::new_command("sycl-ls");
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    match crate::common::subprocess::run_with_cancel(&mut cmd, cancel).await {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            text.lines()
                .filter(|l| l.contains("Intel"))
                .map(|line| GpuInfo {
                    vendor: GpuVendor::Intel,
                    model: line.trim().to_string(),
                    vram_mb: None,
                    driver_version: None,
                    cuda_version: None,
                    rocm_version: None,
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

// ===== Apple =====

#[cfg(target_os = "macos")]
async fn detect_apple(cancel: &CancellationToken) -> Vec<GpuInfo> {
    let mut cmd = crate::common::process_util::new_command("system_profiler");
    cmd.args(["SPDisplaysDataType", "-json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    match crate::common::subprocess::run_with_cancel(&mut cmd, cancel).await {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            parse_apple_json(&text)
        }
        _ => Vec::new(),
    }
}

#[cfg(not(target_os = "macos"))]
async fn detect_apple(_cancel: &CancellationToken) -> Vec<GpuInfo> {
    Vec::new()
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
