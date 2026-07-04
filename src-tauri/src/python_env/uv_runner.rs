//! uv 命令封装
//!
//! 设计模式：**Adapter** - 将外部 `uv` 二进制命令封装为 Rust 接口
//!
//! 详见 `PR/03-模块设计/02-PythonEnvManager.md §1 模块职责`
//!
//! uv 子命令：
//! - `uv python install <version>`：安装便携 Python
//! - `uv venv <path> --python <version>`：创建 venv
//! - `uv pip install <packages>`：装包（torch / requirements.txt）

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::EnvError;

/// uv 子进程超时（秒）—— torch 安装可能很久，给 600s
const UV_TIMEOUT_SECS: u64 = 600;

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
    pub async fn is_available(&self) -> bool {
        if self.uv_binary == PathBuf::from("uv") {
            // 通过 PATH 查找
            tokio::process::Command::new(&self.uv_binary)
                .arg("--version")
                .output()
                .await
                .map(|o| o.status.success())
                .unwrap_or(false)
        } else {
            self.uv_binary.exists()
        }
    }

    /// 安装便携 Python
    ///
    /// `uv python install <version>`
    pub async fn install_python(&self, version: &str) -> Result<(), EnvError> {
        let output = self
            .run_cmd(&["python", "install", version])
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
    /// `uv venv <path> --python <version>`
    pub async fn create_venv(
        &self,
        venv_path: &Path,
        python_version: &str,
    ) -> Result<(), EnvError> {
        let venv_str = venv_path.to_string_lossy().to_string();
        let python_arg = format!("--python={}", python_version);
        let output = self
            .run_cmd(&["venv", &venv_str, &python_arg])
            .await
            .map_err(|e| EnvError::VenvCreateFailed(e.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(EnvError::VenvCreateFailed(stderr.into_owned()));
        }
        tracing::info!(?venv_path, python = python_version, "venv created");
        Ok(())
    }

    /// 安装 torch（按 CUDA 版本构造索引 URL）
    ///
    /// `uv pip install torch torchvision torchaudio --index-url <url>`
    pub async fn install_torch(
        &self,
        venv_path: &Path,
        cuda_version: &crate::config::CudaVersion,
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
            .run_cmd(&args_ref)
            .await
            .map_err(|e| EnvError::TorchInstallFailed(e.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(EnvError::TorchInstallFailed(stderr.into_owned()));
        }
        tracing::info!(?cuda_version, "torch installed");
        Ok(())
    }

    /// 安装 requirements.txt
    ///
    /// `uv pip install -r <file> --python <venv>`
    pub async fn install_requirements(
        &self,
        venv_path: &Path,
        requirements_file: &Path,
    ) -> Result<(), EnvError> {
        let venv_arg = format!("--python={}", venv_python_arg(venv_path));
        let req_str = format!("-r={}", requirements_file.to_string_lossy());

        let args: Vec<&str> = vec!["pip", "install", &venv_arg, &req_str];
        let output = self
            .run_cmd(&args)
            .await
            .map_err(|e| EnvError::RequirementsInstallFailed(e.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(EnvError::RequirementsInstallFailed(stderr.into_owned()));
        }
        tracing::info!(?requirements_file, "requirements installed");
        Ok(())
    }

    /// 执行 uv 子命令（带超时）
    async fn run_cmd(&self, args: &[&str]) -> Result<std::process::Output, std::io::Error> {
        tokio::time::timeout(
            Duration::from_secs(UV_TIMEOUT_SECS),
            tokio::process::Command::new(&self.uv_binary).args(args).output(),
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "uv subprocess timeout");
            std::io::Error::new(std::io::ErrorKind::TimedOut, e.to_string())
        })?
    }
}

/// 构造 venv 的 python 参数值（跨平台）
///
/// `uv pip install --python=<venv>/Scripts/python.exe` 或 `<venv>/bin/python`
fn venv_python_arg(venv_path: &Path) -> String {
    let python = if cfg!(windows) {
        venv_path.join("Scripts").join("python.exe")
    } else {
        venv_path.join("bin").join("python")
    };
    python.to_string_lossy().into_owned()
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
