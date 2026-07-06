//! uv 命令封装
//!
//! 设计模式：**Adapter** - 将外部 `uv` 二进制命令封装为 Rust 接口
//!
//! 详见 `PR/03-模块设计/02-PythonEnvManager.md §1 模块职责`
//!
//! v3.6：所有子进程调用用 `CancellationToken` 替代 `tokio::time::timeout`，
//! 不再有 600s 硬性超时，用户可通过 CancellationToken 主动取消。

use std::path::{Path, PathBuf};

use tokio_util::sync::CancellationToken;

use crate::error::EnvError;

/// uv 命令执行器
pub struct UvRunner {
    /// uv 二进制路径（首次运行由 launcher 释放到用户数据目录）
    uv_binary: PathBuf,
}

impl UvRunner {
    pub fn new(uv_binary: PathBuf) -> Self {
        Self { uv_binary }
    }

    /// 默认构造：在 PATH 中查找 uv
    pub fn from_path() -> Self {
        Self {
            uv_binary: PathBuf::from("uv"),
        }
    }

    /// uv 二进制路径
    pub fn binary_path(&self) -> &Path {
        &self.uv_binary
    }

    /// 检测 uv 是否可用
    ///
    /// 快速命令（<1s），不需要 cancel
    pub async fn is_available(&self) -> bool {
        let uv_bin = &self.uv_binary;
        if uv_bin == &PathBuf::from("uv") {
            crate::common::process_util::new_command(uv_bin)
                .arg("--version")
                .output()
                .await
                .map(|o| o.status.success())
                .unwrap_or(false)
        } else {
            if !uv_bin.exists() {
                return false;
            }
            crate::common::process_util::new_command(uv_bin)
                .arg("--version")
                .output()
                .await
                .map(|o| o.status.success())
                .unwrap_or(false)
        }
    }

    /// 获取 uv 版本字符串（v2.13）
    ///
    /// 快速命令（<1s），不需要 cancel
    pub async fn get_version(&self) -> (Option<String>, bool) {
        let uv_bin = &self.uv_binary;
        let is_absolute = uv_bin != &PathBuf::from("uv") && uv_bin.is_absolute();
        if is_absolute && !uv_bin.exists() {
            return (None, false);
        }
        match crate::common::process_util::new_command(uv_bin).arg("--version").output().await {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let version = stdout
                    .trim()
                    .split_whitespace()
                    .nth(1)
                    .map(|s| s.to_string());
                (version, true)
            }
            _ => (None, false),
        }
    }

    /// 安装便携 Python
    ///
    /// v3.6：接 `CancellationToken`
    pub async fn install_python(
        &self,
        version: &str,
        cancel: &CancellationToken,
    ) -> Result<(), EnvError> {
        let output = self
            .run_cmd(&["python", "install", version], cancel)
            .await
            .map_err(|e| EnvError::PythonInstallFailed(e.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(EnvError::PythonInstallFailed(stderr.into_owned()));
        }
        tracing::info!(version, "portable python installed");
        Ok(())
    }

    /// 创建 venv
    ///
    /// v3.6：接 `CancellationToken`
    pub async fn create_venv(
        &self,
        venv_path: &Path,
        python_version: &str,
        cancel: &CancellationToken,
    ) -> Result<(), EnvError> {
        if !self.is_available().await {
            return Err(EnvError::UvNotFound(self.uv_binary.to_string_lossy().into_owned()));
        }

        if venv_path.exists() {
            tracing::info!(
                ?venv_path,
                "create_venv: removing existing directory before creation"
            );
            tokio::fs::remove_dir_all(venv_path)
                .await
                .map_err(|e| EnvError::VenvCreateFailed(format!(
                    "failed to remove existing venv directory: {}\nvenv 路径: {}\n提示: 可能有进程占用 venv 目录（如 python.exe 残留），请关闭相关程序后重试",
                    e, venv_path.display()
                )))?;
        }

        let venv_str = venv_path.to_string_lossy().to_string();
        let python_arg = format!("--python={}", python_version);
        let output = match self.run_cmd(&["venv", &venv_str, &python_arg, "--seed"], cancel).await {
            Ok(out) => out,
            Err(e) => {
                return Err(EnvError::VenvCreateFailed(format!(
                    "{}\nvenv 路径: {}\nPython 版本: {}\n提示: 请检查 uv 是否在 PATH 中，或在「设置」中指定 uv 路径",
                    e, venv_str, python_version
                )));
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(EnvError::VenvCreateFailed(format!(
                "{}\nvenv 路径: {}\nPython 版本: {}",
                stderr, venv_str, python_version
            )));
        }
        tracing::info!(?venv_path, python = python_version, "venv created");
        Ok(())
    }

    /// 安装 torch（按 CUDA 版本构造索引 URL）
    ///
    /// v3.6：接 `CancellationToken`，透传给 `run_cmd` 和 `smoke_test_torch`
    pub async fn install_torch(
        &self,
        venv_path: &Path,
        cuda_version: &crate::config::CudaVersion,
        cancel: &CancellationToken,
    ) -> Result<(), EnvError> {
        let venv_arg = format!("--python={}", venv_python_arg(venv_path));
        let index_url = cuda_index_url(cuda_version);

        let mut args: Vec<String> = vec![
            "pip".into(),
            "install".into(),
            venv_arg,
            "torch".into(),
            "torchvision".into(),
            "torchaudio".into(),
        ];
        if let Some(url) = index_url {
            args.push("--index-url".into());
            args.push(url);
        }

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let output = self
            .run_cmd(&args_ref, cancel)
            .await
            .map_err(|e| EnvError::TorchInstallFailed(e.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(EnvError::TorchInstallFailed(stderr.into_owned()));
        }

        // smoke test 也需要 cancel
        self.smoke_test_torch(venv_path, cancel).await?;

        tracing::info!(?cuda_version, "torch installed and smoke-tested");
        Ok(())
    }

    /// v1.8 关键：torch smoke test
    ///
    /// v3.6：接 `CancellationToken`，透传给 `probe_torch_script`
    pub async fn smoke_test_torch(
        &self,
        venv_path: &Path,
        cancel: &CancellationToken,
    ) -> Result<(), EnvError> {
        let python = venv_python_path(venv_path);
        if !python.exists() {
            return Err(EnvError::TorchInstallFailed(format!(
                "venv python 不存在: {}（smoke test 无法运行）",
                python.display()
            )));
        }

        let json_output = crate::env_inspector::scripts::probe_torch_script(venv_path, cancel)
            .await
            .map_err(|e| {
                EnvError::TorchInstallFailed(format!("smoke test 启动失败: {}", e))
            })?;

        let probe = crate::env_inspector::scripts::parse_torch_probe(&json_output);
        if !probe.installed {
            let err_type = probe.error_type.as_deref().unwrap_or("Unknown");
            let err_msg = probe.error_msg.as_deref().unwrap_or("(无错误信息)");
            let tb = probe.traceback_tail.as_deref().unwrap_or("");
            return Err(EnvError::TorchInstallFailed(format!(
                "torch 安装后 smoke test 失败：{}: {}\n\n\
                 这通常意味着 torch wheel 装好了，但某个关键依赖（如 numpy）有问题。\n\
                 traceback 末尾:\n{}\n\n\
                 建议：尝试「环境修复」→ 重新安装 PyTorch，或手动降级 numpy",
                err_type, err_msg, tb
            )));
        }

        tracing::info!(
            torch_version = %probe.version.as_deref().unwrap_or("?"),
            "torch smoke test passed"
        );
        Ok(())
    }

    /// 切换 torch 变体（v3.0 新增，F25）
    ///
    /// v3.6：接 `CancellationToken`
    pub async fn install_torch_variant(
        &self,
        venv_path: &Path,
        variant: &crate::python_env::TorchVariant,
        cancel: &CancellationToken,
    ) -> Result<(), EnvError> {
        let venv_arg = format!("--python={}", venv_python_arg(venv_path));
        let (pkgs, index_url) = variant.install_args();

        let mut args: Vec<String> = vec![
            "pip".into(),
            "install".into(),
            "--upgrade".into(),
            venv_arg,
        ];
        for pkg in &pkgs {
            args.push(pkg.clone());
        }
        if let Some(url) = index_url {
            args.push("--index-url".into());
            args.push(url);
        }

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        tracing::info!(?variant, "switching torch variant");

        let output = self
            .run_cmd(&args_ref, cancel)
            .await
            .map_err(|e| EnvError::TorchInstallFailed(e.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(EnvError::TorchInstallFailed(format!(
                "切换 torch 变体失败 ({}): {}",
                variant.label(),
                stderr
            )));
        }

        // 验证：python -c "<verify>"
        let python = venv_python_path(venv_path);
        if python.exists() {
            let verify = variant.verify_command();
            let mut verify_cmd = crate::common::process_util::new_command(&python);
            verify_cmd
                .arg("-c")
                .arg(verify)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true);

            let verify_output = crate::common::subprocess::run_with_cancel(&mut verify_cmd, cancel)
                .await
                .map_err(|e| EnvError::TorchInstallFailed(format!("验证命令启动失败: {}", e)))?;
            if !verify_output.status.success() {
                let stderr = String::from_utf8_lossy(&verify_output.stderr);
                return Err(EnvError::TorchInstallFailed(format!(
                    "torch 切换后验证失败 ({}): {}",
                    variant.label(),
                    stderr.trim()
                )));
            }
        }

        tracing::info!(?variant, "torch variant installed and verified");
        Ok(())
    }

    /// 安装 requirements.txt
    ///
    /// v3.6：接 `CancellationToken`
    pub async fn install_requirements(
        &self,
        venv_path: &Path,
        requirements_file: &Path,
        constraints: Option<&Path>,
        cancel: &CancellationToken,
    ) -> Result<(), EnvError> {
        let venv_arg = format!("--python={}", venv_python_arg(venv_path));
        let req_str = format!("-r={}", requirements_file.to_string_lossy());

        let mut args: Vec<String> = vec![
            "pip".into(),
            "install".into(),
            venv_arg,
            req_str,
        ];
        if let Some(c) = constraints {
            args.push("-c".into());
            args.push(c.to_string_lossy().into_owned());
        }
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let output = self
            .run_cmd(&args_ref, cancel)
            .await
            .map_err(|e| EnvError::RequirementsInstallFailed(e.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(EnvError::RequirementsInstallFailed(stderr.into_owned()));
        }
        tracing::info!(?requirements_file, ?constraints, "requirements installed");
        Ok(())
    }

    /// v1.8 / F36：强制按新 constraints 重装 requirements
    ///
    /// v3.6：接 `CancellationToken`
    pub async fn install_requirements_upgrade(
        &self,
        venv_path: &Path,
        requirements_file: &Path,
        constraints: Option<&Path>,
        cancel: &CancellationToken,
    ) -> Result<(), EnvError> {
        let venv_arg = format!("--python={}", venv_python_arg(venv_path));
        let req_str = format!("-r={}", requirements_file.to_string_lossy());

        let mut args: Vec<String> = vec![
            "pip".into(),
            "install".into(),
            "--upgrade".into(),
            "--force-reinstall".into(),
            venv_arg,
            req_str,
        ];
        if let Some(c) = constraints {
            args.push("-c".into());
            args.push(c.to_string_lossy().into_owned());
        }
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let output = self
            .run_cmd(&args_ref, cancel)
            .await
            .map_err(|e| EnvError::RequirementsInstallFailed(e.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(EnvError::RequirementsInstallFailed(stderr.into_owned()));
        }
        tracing::info!(?requirements_file, ?constraints, "requirements upgraded with --force-reinstall");
        Ok(())
    }

    /// 执行 uv 子命令（带 CancellationToken）
    ///
    /// v3.6：用 `subprocess::run_with_cancel` 替代 `tokio::time::timeout`
    async fn run_cmd(
        &self,
        args: &[&str],
        cancel: &CancellationToken,
    ) -> Result<std::process::Output, std::io::Error> {
        let mut cmd = crate::common::process_util::new_command(&self.uv_binary);
        cmd.args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        crate::common::subprocess::run_with_cancel(&mut cmd, cancel)
            .await
            .map_err(|e| match e {
                crate::common::subprocess::SubprocessError::Io(io_err) => io_err,
                crate::common::subprocess::SubprocessError::Cancelled => {
                    std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled")
                }
                crate::common::subprocess::SubprocessError::Exit { code, stderr } => {
                    std::io::Error::new(std::io::ErrorKind::Other, format!("exit {}: {}", code, stderr))
                }
            })
    }
}

/// 构造 venv 的 python 参数值（跨平台）
fn venv_python_arg(venv_path: &Path) -> String {
    venv_python_path(venv_path).to_string_lossy().into_owned()
}

/// 构造 venv 的 python 可执行文件完整路径（跨平台）
pub fn venv_python_path(venv_path: &Path) -> PathBuf {
    if cfg!(windows) {
        venv_path.join("Scripts").join("python.exe")
    } else {
        venv_path.join("bin").join("python")
    }
}

/// 根据 CUDA 版本构造 PyTorch 索引 URL
fn cuda_index_url(cuda: &crate::config::CudaVersion) -> Option<String> {
    use crate::config::CudaVersion;
    match cuda {
        CudaVersion::Cpu => None,
        CudaVersion::Cu118 => Some("https://download.pytorch.org/whl/cu118".into()),
        CudaVersion::Cu121 => Some("https://download.pytorch.org/whl/cu121".into()),
        CudaVersion::Cu124 => Some("https://download.pytorch.org/whl/cu124".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CudaVersion;

    #[test]
    fn test_cuda_index_url_cpu_returns_none() {
        assert!(cuda_index_url(&CudaVersion::Cpu).is_none());
    }

    #[test]
    fn test_cuda_index_url_cu121() {
        let url = cuda_index_url(&CudaVersion::Cu121).unwrap();
        assert!(url.contains("cu121"));
    }

    #[test]
    fn test_venv_python_arg_windows() {
        let venv = Path::new("/tmp/venv");
        let arg = venv_python_arg(venv);
        if cfg!(windows) {
            assert!(arg.contains("Scripts"));
        } else {
            assert!(arg.contains("bin"));
        }
    }
}
