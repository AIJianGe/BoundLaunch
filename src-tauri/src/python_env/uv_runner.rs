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
        let uv_bin = &self.uv_binary;
        if uv_bin == &PathBuf::from("uv") {
            // 通过 PATH 查找
            tokio::process::Command::new(uv_bin)
                .arg("--version")
                .output()
                .await
                .map(|o| o.status.success())
                .unwrap_or(false)
        } else {
            // 绝对路径：检查文件存在 + 可执行
            if !uv_bin.exists() {
                return false;
            }
            // 用 --version 实际跑一遍确认可执行
            tokio::process::Command::new(uv_bin)
                .arg("--version")
                .output()
                .await
                .map(|o| o.status.success())
                .unwrap_or(false)
        }
    }

    /// 获取 uv 版本字符串（v2.13）
    ///
    /// 执行 `uv --version`，解析输出格式：`uv <version> (<commit> <date>)`
    /// 返回 `(version_string, is_available)` 元组：
    /// - 可用 + 解析成功 → `(Some("0.4.18"), true)`
    /// - 可用 + 解析失败 → `(None, true)`（输出格式未知时降级）
    /// - 不可用 → `(None, false)`
    pub async fn get_version(&self) -> (Option<String>, bool) {
        let uv_bin = &self.uv_binary;
        let is_absolute = uv_bin != &PathBuf::from("uv") && uv_bin.is_absolute();
        if is_absolute && !uv_bin.exists() {
            return (None, false);
        }
        match tokio::process::Command::new(uv_bin).arg("--version").output().await {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                // 格式：`uv 0.4.18 (d3dc3a323 2024-11-21)`
                // 取第二个空格分隔的 token
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
    /// `uv venv <path> --python <version> --seed`
    ///
    /// `--seed` 参数让 uv 在 venv 中安装 pip + setuptools + wheel，原因：
    /// - uv venv 默认不装 pip（uv 自己是包管理器，不需要 pip）
    /// - 但项目 inspect_dependencies / verify_venv 流程依赖 `python -m pip list`
    /// - ComfyUI 运行时某些自定义节点可能用 pip 装依赖
    ///
    /// **v2.12：创建前清理已存在目录**
    /// uv venv 不允许在已存在的目录上创建（即使目录不是合法 venv）。
    /// 上次失败的 venv 创建可能留下不完整目录（如缺 pyvenv.cfg），
    /// 导致用户重试时 uv 报 "exists, but it's not a virtual environment"。
    /// 一律先删除，确保从干净状态开始。
    ///
    /// 错误处理：uv 不存在时直接返回 `UvNotFound`，避免把底层 `program not found`
    /// 错误归为 venv 创建失败。
    pub async fn create_venv(
        &self,
        venv_path: &Path,
        python_version: &str,
    ) -> Result<(), EnvError> {
        // 先确认 uv 可用 — 避免把「uv not found」包成「venv 创建失败」
        if !self.is_available().await {
            return Err(EnvError::UvNotFound(self.uv_binary.to_string_lossy().into_owned()));
        }

        // v2.12：创建前清理已存在的目录
        //
        // 场景：上次 create_venv 失败（超时 / kill / 中断）可能留下不完整目录，
        // uv 检测到目录存在但不是合法 venv 时会直接报错退出。
        // 这里无条件先删除，保证从干净状态开始。
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
        let output = match self.run_cmd(&["venv", &venv_str, &python_arg, "--seed"]).await {
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

    /// 切换 torch 变体（v3.0 新增，F25）
    ///
    /// 5 厂商（NVIDIA / AMD / Intel / Apple / CPU）统一通过 `TorchVariant` 抽象。
    ///
    /// 实现要点：
    /// - 用 `uv pip install --upgrade` 而非 `uninstall + install`
    ///   原因：uninstall torch 会同时移除 torchvision / torchaudio 等依赖它的包，
    ///   而 --upgrade 让 uv 智能检测现有版本，按需升级/降级/重装，保留其他包。
    /// - 安装后调 `variant.verify_command()` 验证 torch 能 import + 设备可用
    /// - 失败时返回 Err，旧 torch 保留（不破坏 venv）
    pub async fn install_torch_variant(
        &self,
        venv_path: &Path,
        variant: &crate::python_env::TorchVariant,
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
            .run_cmd(&args_ref)
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
            let verify_output = tokio::process::Command::new(&python)
                .arg("-c")
                .arg(verify)
                .output()
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
    venv_python_path(venv_path).to_string_lossy().into_owned()
}

/// 构造 venv 的 python 可执行文件完整路径（跨平台）
///
/// `<venv>/Scripts/python.exe` (Windows) / `<venv>/bin/python` (Unix)
fn venv_python_path(venv_path: &Path) -> PathBuf {
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
