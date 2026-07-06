//! GPU 检测
//!
//! 详见 `PR/03-模块设计/07-EnvironmentInspector.md §5 数据流` 中 detect_gpu 子任务
//!
//! 策略：
//! 1. 优先调 `nvidia-smi --query-gpu=name,memory.total,driver_version --format=csv,noheader`
//! 2. 无 nvidia-smi → 从 torch.cuda 推断（GpuInfo::Nvidia 仅 name 已知）
//! 3. 仍无 → GpuInfo::CpuOnly（从 cpu brand 推断）

use std::time::Duration;

use super::models::GpuInfo;

/// nvidia-smi 子进程超时（秒）
const NVIDIA_SMI_TIMEOUT_SECS: u64 = 5;

/// nvidia-smi 命令行参数
const NVIDIA_SMI_ARGS: &[&str] = &[
    "--query-gpu=name,memory.total,driver_version",
    "--format=csv,noheader,nounits",
];

/// 检测 GPU
///
/// 永远返回 GpuInfo（不报错），失败时降级为 CpuOnly / Unknown
pub async fn detect_gpu() -> GpuInfo {
    match try_detect_nvidia().await {
        Some(gpu) => gpu,
        None => {
            // 无 NVIDIA，目前不做 AMD/Intel 子进程检测（暂未实现），
            // 直接降级为 CpuOnly（从 CPU 型号获取）
            GpuInfo::CpuOnly {
                cpu_model: detect_cpu_model(),
            }
        }
    }
}

/// 尝试调用 nvidia-smi
async fn try_detect_nvidia() -> Option<GpuInfo> {
    // v3.3：使用 new_command 在 Windows 上加 CREATE_NO_WINDOW，避免弹 cmd 窗口
    let output = tokio::time::timeout(
        Duration::from_secs(NVIDIA_SMI_TIMEOUT_SECS),
        crate::common::process_util::new_command("nvidia-smi")
            .args(NVIDIA_SMI_ARGS)
            .output(),
    )
    .await
    .ok()?
    .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_nvidia_smi_output(&stdout)
}

/// 解析 `nvidia-smi` 输出
///
/// 输入格式：`NVIDIA GeForce RTX 4090, 24564, 551.23`
fn parse_nvidia_smi_output(stdout: &str) -> Option<GpuInfo> {
    let line = stdout.lines().next()?;
    let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
    if parts.len() < 3 {
        return None;
    }
    let name = parts[0].to_string();
    let memory_mb: u64 = parts[1].parse().ok()?;
    let driver_version = parts[2].to_string();
    Some(GpuInfo::Nvidia {
        name,
        memory_mb,
        driver_version,
    })
}

/// 检测 CPU 型号（用于 CpuOnly 降级）
fn detect_cpu_model() -> String {
    // 简化实现：通过环境变量获取（跨平台）
    #[cfg(target_os = "windows")]
    {
        std::env::var("PROCESSOR_IDENTIFIER")
            .unwrap_or_else(|_| "Unknown CPU".to_string())
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|content| {
                content
                    .lines()
                    .find(|l| l.starts_with("model name"))
                    .and_then(|l| l.split(':').nth(1).map(|s| s.trim().to_string()))
            })
            .unwrap_or_else(|| "Unknown CPU".to_string())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("sysctl")
            .arg("-n")
            .arg("machdep.cpu.brand_string")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "Apple Silicon".to_string())
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        "Unknown CPU".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_nvidia_smi_single_line() {
        let stdout = "NVIDIA GeForce RTX 4090, 24564, 551.23\n";
        let gpu = parse_nvidia_smi_output(stdout).unwrap();
        match gpu {
            GpuInfo::Nvidia {
                name,
                memory_mb,
                driver_version,
            } => {
                assert_eq!(name, "NVIDIA GeForce RTX 4090");
                assert_eq!(memory_mb, 24564);
                assert_eq!(driver_version, "551.23");
            }
            _ => panic!("expected Nvidia variant"),
        }
    }

    #[test]
    fn test_parse_nvidia_smi_multi_line_uses_first() {
        // 多 GPU 场景，目前只取第一块
        let stdout = "NVIDIA GeForce RTX 4090, 24564, 551.23\nNVIDIA GeForce RTX 3080, 10240, 551.23\n";
        let gpu = parse_nvidia_smi_output(stdout).unwrap();
        match gpu {
            GpuInfo::Nvidia { name, .. } => assert_eq!(name, "NVIDIA GeForce RTX 4090"),
            _ => panic!("expected Nvidia variant"),
        }
    }

    #[test]
    fn test_parse_nvidia_smi_empty_returns_none() {
        assert!(parse_nvidia_smi_output("").is_none());
    }

    #[test]
    fn test_parse_nvidia_smi_malformed_returns_none() {
        assert!(parse_nvidia_smi_output("bogus line").is_none());
    }

    #[test]
    fn test_detect_cpu_model_returns_nonempty() {
        let model = detect_cpu_model();
        assert!(!model.is_empty());
    }
}
