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

    /// 安装 torch 主包（v3.7：F3 拆分 3 阶段，本函数对应阶段 1/2/3 之一）
    ///
    /// **v3.8 D-L1 改造**：拆成两条独立 uv 命令：
    /// 1. `uv pip install --index-url <pytorch.org> torch`（仅 torch，单独装）
    /// 2. `uv pip install --index-url <pytorch.org> --no-deps --upgrade torchvision torchaudio`（vision/audio 用 `--no-deps` 绕开 torch 依赖约束）
    ///
    /// **为什么拆两步**：
    /// - 用户环境曾出现过 "torch 2.11.0+cu128 装对了，但 torchvision 被解析到 yanked 远古版 0.1.6（2017 年）"
    ///   的 bug（详见 PR/03-模块设计/02-PythonEnvManager.md §11 历史事故 2026-07-07）
    /// - 根因：uv 解析 `torch torchvision torchaudio` 时把 torchvision 的依赖约束传到 PyPI metadata
    ///   解析层，PyPI 上没 0.22.0+cu128 的 wheel → 退回到 0.1.6 yanked 包（uv 0.5+ 仍允许装 yanked）
    /// - 修复：vision/audio 单独装 + `--no-deps` 切断 torch 约束链 + `--upgrade` 强制用最新
    ///   + `--index-url` 严格只在 pytorch.org 上找（uv 不会再 fallback 到 PyPI）
    ///
    /// torchvision 的可选依赖（six / av / Pillow / pycocotools）由 [`install_torch_extras`] 装。
    ///
    /// v3.6：接 `CancellationToken`，透传给 `run_cmd` 和 `smoke_test_torch`
    /// v3.7（F1）：返回 `Result<(), EnvError>`，移除内嵌的 six/av 补充（移到 extras）
    /// v3.7（F3）：拆出 3 阶段后，本函数成为 install_torch_stage_torch（0..=70%）
    /// v3.7（F4）：可选 line_collector 实时日志（None = 不收集）
    /// v3.8（D-L1）：拆成两条 uv 命令，vision/audio 用 `--no-deps --upgrade`
    /// v3.8（D-L2）：装完调 `verify_torchvision_filesystem` 校验子目录和 _C.pyd
    /// v3.8（D-L4）：校验失败时自动调 `reinstall_torchvision_torchaudio` 强制重装
    pub async fn install_torch(
        &self,
        venv_path: &Path,
        cuda_version: &crate::config::CudaVersion,
        cancel: &CancellationToken,
        line_collector: Option<&std::sync::Arc<crate::common::line_collector::LineCollector>>,
    ) -> Result<(), EnvError> {
        let venv_arg = format!("--python={}", venv_python_arg(venv_path));
        let index_url = cuda_index_url(cuda_version);

        // ========== 步骤 1/2：装 torch（--index-url pytorch.org）==========
        let mut args: Vec<String> = vec![
            "pip".into(),
            "install".into(),
            venv_arg.clone(),
            "torch".into(),
        ];
        if let Some(ref url) = index_url {
            args.push("--index-url".into());
            args.push(url.clone());
        }
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        tracing::info!(?cuda_version, "install_torch: 步骤 1/2 - 装 torch 主包");
        let output = self
            .run_uv_pip(
                &args_ref,
                cancel,
                line_collector,
                "uv:torch",
            )
            .await
            .map_err(|e| EnvError::TorchInstallFailed(e.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(EnvError::TorchInstallFailed(format!(
                "torch 主包安装失败: {}",
                stderr
            )));
        }

        if cancel.is_cancelled() {
            return Err(EnvError::Cancelled);
        }

        // ========== 步骤 2/2：装 torchvision + torchaudio（--no-deps --upgrade）==========
        // 关键：`--no-deps` 切断 vision/audio 解析时的 torch 依赖约束
        //       `--upgrade` 强制 uv 选 pytorch.org 上最新版本（避免 pip 缓存中的旧版本）
        //       `--index-url` 严格只在 pytorch.org 上找（uv 不会 fallback 到 PyPI）
        //
        // **v3.9 关键修复**：index_url 必须存在
        // - 之前 `cuda_index_url(Cpu)` 返回 None，导致步骤 2 走 PyPI 默认源
        // - PyPI 镜像了 pytorch.org 的 cu128 wheel → 装出 `torchaudio+cu128`
        // - 但 torch 是 cpu 版 → 版本混乱 → ComfyUI 启动报 "Torch not compiled with CUDA enabled"
        // - 现在 `cuda_index_url` 永远返回 Some（Cpu → pytorch.org CPU 源）
        // - 这里用 `let Some(url) = ... else { return Err(...) }` 强制把关，避免未来再次回归
        let url2 = match &index_url {
            Some(u) => u.clone(),
            None => {
                return Err(EnvError::TorchInstallFailed(format!(
                    "install_torch 步骤 2: cuda_index_url 返回 None（cuda_version={:?}），\
                     这是 bug！应该是 v3.9 之前遗漏的逻辑：CPU 模式必须从 pytorch.org CPU 源装，\
                     避免 PyPI 镜像的 cu128 wheel 混入",
                    cuda_version
                )));
            }
        };
        let args2: Vec<String> = vec![
            "pip".into(),
            "install".into(),
            venv_arg,
            "--no-deps".into(),
            "--upgrade".into(),
            "torchvision".into(),
            "torchaudio".into(),
            "--index-url".into(),
            url2,
        ];
        let args2_ref: Vec<&str> = args2.iter().map(|s| s.as_str()).collect();
        tracing::info!(
            ?cuda_version,
            "install_torch: 步骤 2/2 - 装 torchvision + torchaudio（--no-deps --upgrade --index-url pytorch.org）"
        );
        let output2 = self
            .run_uv_pip(
                &args2_ref,
                cancel,
                line_collector,
                "uv:torch-vision",
            )
            .await
            .map_err(|e| EnvError::TorchInstallFailed(e.to_string()))?;
        if !output2.status.success() {
            let stderr = String::from_utf8_lossy(&output2.stderr);
            return Err(EnvError::TorchInstallFailed(format!(
                "torchvision/torchaudio 安装失败: {}",
                stderr
            )));
        }

        if cancel.is_cancelled() {
            return Err(EnvError::Cancelled);
        }

        // ========== v3.8 D-L2：文件系统校验 ==========
        // 检查 site-packages/torchvision/ops/ 和 _C.pyd 是否存在
        // 不存在 → 触发 v3.8 D-L4 自动重装
        match verify_torchvision_filesystem(venv_path) {
            Ok(()) => {
                tracing::info!("verify_torchvision_filesystem: 子模块完整，跳过自动重装");
            }
            Err(reason) => {
                tracing::warn!(
                    reason = %reason,
                    "verify_torchvision_filesystem: torchvision 子模块残缺，触发自动重装"
                );
                // v3.8 D-L4：自动重装 torchvision/torchaudio
                reinstall_torchvision_torchaudio(
                    self,
                    venv_path,
                    cuda_version,
                    cancel,
                    line_collector,
                )
                .await?;
                // 重装后再校验一次
                verify_torchvision_filesystem(venv_path).map_err(|e2| {
                    EnvError::TorchInstallFailed(format!(
                        "torchvision 自动重装后仍残缺: {}（第 2 次重装后问题未解决，\
                         请检查 pytorch.org 上是否有匹配 cu{:?} 的 torchvision wheel）",
                        e2, cuda_version
                    ))
                })?;
            }
        }

        // smoke test 也需要 cancel
        self.smoke_test_torch(venv_path, cancel).await?;

        tracing::info!(?cuda_version, "torch installed and smoke-tested");
        Ok(())
    }

    /// v3.7（F1 新增）：装 torchvision 需要的可选依赖 + torch 关键运行依赖
    ///
    /// ## 解决的三个问题
    ///
    /// ### 1. `ModuleNotFoundError: No module named 'six'`（F1 原始问题）
    /// torchvision 0.20 之前用 `six` 兼容 Py2/3，新版仍保留对 `lsun` 等数据集子包的引用。
    /// ComfyUI 实际不会用 `lsun` 这类数据集子包，但 `import torchvision` 会触发 `datasets` 整包加载。
    /// 装 `six / av / Pillow / pycocotools`（torchvision 实际用到的可选依赖），
    /// 这样后续 `from torchvision import datasets` 不会因为缺 `six` 而炸。
    ///
    /// ### 2. `AssertionError: Torch not compiled with CUDA enabled`（v3.9 历史误判的修正）
    /// **v3.9 之前的分析（部分正确）**：
    /// - 当时认为 venv 中缺 `numpy`，`import torch` 触发 deprecation 警告混入 `_lazy_init()` 误判
    /// - 加 `numpy + psutil` 是**防御性补装**，对真因无直接修复作用
    ///
    /// **v3.9 真正的根因**（详见 `cuda_index_url` 函数注释）：
    /// - `cuda_index_url(Cpu)` 之前返回 `None`，导致 install_torch 步骤 2 不传 `--index-url`
    /// - uv 走 PyPI 默认源，PyPI 镜像了 pytorch.org 的 cu128 wheel
    /// - 结果：venv 里 `torch+cpu` + `torchaudio+cu128` + `torchvision+cu128` → **版本混乱**
    /// - ComfyUI 启动时 `torch.cuda.is_available() → False`（torch 真的没 CUDA）
    ///
    /// **v3.9 真正修复**（不在本函数，在 `cuda_index_url`）：
    /// - CPU 模式也走 pytorch.org CPU 源 `https://download.pytorch.org/whl/cpu`
    /// - 步骤 1 和步骤 2 都强制走 pytorch.org，保证 torch/torchvision/torchaudio 版本一致
    ///
    /// **v3.9 防御性补装**：保留 numpy + psutil 显式装
    /// - numpy 是 torch 内部 import 路径
    /// - psutil 是 ComfyUI `model_management.py` 直接 import
    /// - 不装这两个会触发别的边缘问题（即使不是这个 AssertionError 的根因）
    ///
    /// v3.7：接 `CancellationToken`，透传给 `run_cmd`
    /// 用 `--upgrade` 已是最新版的包无副作用（uv 会跳过）
    /// v3.7（F4）：可选 line_collector
    /// v3.9：保留 numpy + psutil 作为防御性补装
    pub async fn install_torch_extras(
        &self,
        venv_path: &Path,
        cancel: &CancellationToken,
        line_collector: Option<&std::sync::Arc<crate::common::line_collector::LineCollector>>,
    ) -> Result<(), EnvError> {
        let venv_arg = format!("--python={}", venv_python_arg(venv_path));
        // v3.9 关键改造：分组注释，便于维护和后续扩展
        // - torchvision 可选依赖（F1）：six/av/Pillow/pycocotools
        // - torch 初始化关键依赖（v3.9 新增）：numpy/psutil
        //   缺 numpy 触发 "Torch not compiled with CUDA enabled" 误判
        //   缺 psutil 触发 ComfyUI model_management.py import 链中段
        let extras = ["six", "av", "Pillow", "pycocotools", "numpy", "psutil"];
        let pkg_specs: Vec<String> = extras.iter().map(|s| s.to_string()).collect();

        let mut args: Vec<String> = vec![
            "pip".into(),
            "install".into(),
            venv_arg,
            "--upgrade".into(),
        ];
        for pkg in &pkg_specs {
            args.push(pkg.clone());
        }

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        tracing::info!(
            packages = ?pkg_specs,
            "installing torchvision extras + torch 关键依赖 (six/av/Pillow/pycocotools/numpy/psutil)"
        );

        let output = if let Some(collector) = line_collector {
            self.run_cmd_with_log(&args_ref, cancel, collector, "uv:torch-extras")
                .await
                .map_err(|e| EnvError::TorchInstallFailed(format!("extras install io error: {}", e)))?
        } else {
            self.run_cmd(&args_ref, cancel)
                .await
                .map_err(|e| EnvError::TorchInstallFailed(format!("extras install io error: {}", e)))?
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(EnvError::TorchInstallFailed(format!(
                "torchvision 可选依赖安装失败: {}",
                stderr.trim()
            )));
        }

        tracing::info!("torchvision extras + torch 关键依赖 installed");
        Ok(())
    }

    /// v3.7（F2 新增）：`uv pip check` 校验 venv 是否有缺包，返回缺包名列表
    ///
    /// `uv pip check` 的输出（缺包时）：
    /// ```text
    /// Resolved 247 packages in 12ms
    /// Audited 247 packages in 3ms
    ///
    /// Found 2 broken packages:
    ///   - six (required by: torchvision)
    ///   - av (required by: torchvision)
    /// ```
    ///
    /// 返回值：`Some(missing)` = 有缺包，列出包名；`None` = 全部正常
    ///
    /// v3.7：5 秒内快查（pip list 失败已记录 warn），不会卡主流程
    pub async fn check_missing_deps(
        &self,
        venv_path: &Path,
    ) -> Result<Option<Vec<String>>, EnvError> {
        let venv_arg = format!("--python={}", venv_python_arg(venv_path));
        let args: Vec<&str> = vec!["pip", "check", &venv_arg];

        // 5 秒超时保护（uv pip check 极快，正常 1-2s）
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.run_cmd(&args, &tokio_util::sync::CancellationToken::new()),
        )
        .await
        .map_err(|_| EnvError::VerifyFailed("uv pip check 超时（5s）".into()))?
        .map_err(|e| EnvError::VerifyFailed(format!("uv pip check IO 错误: {}", e)))?;

        if output.status.success() {
            // exit 0 = 没有 broken packages
            return Ok(None);
        }

        // 解析 stderr 找缺包名
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let combined = format!("{}\n{}", stdout, stderr);

        let mut missing: Vec<String> = Vec::new();
        for line in combined.lines() {
            // 典型格式：`  - six (required by: torchvision)` 或 `Missing dependencies: six`
            if let Some(rest) = line.trim_start().strip_prefix("- ") {
                // 截断到 ` (` 或行尾
                let pkg = rest
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_' && c != '.')
                    .to_string();
                if !pkg.is_empty() {
                    missing.push(pkg);
                }
            }
        }

        if missing.is_empty() {
            // 兜底：整段输出当作 "Other"，让上游看 stderr 诊断
            return Err(EnvError::RequirementsInstallFailed(format!(
                "uv pip check 返回非零退出但未解析到缺包名：\n{}",
                combined.trim()
            )));
        }

        // 去重 + 排序
        missing.sort();
        missing.dedup();
        Ok(Some(missing))
    }

    /// v1.8 关键：torch smoke test
    ///
    /// v3.6：接 `CancellationToken`，透传给 `probe_torch_script`
    /// v3.8 D-INTEGRATE：torchvision 子模块校验失败时自动触发 D-L4 重装
    ///
    /// **校验流程**：
    /// 1. 跑 `probe_torch_script`（同时检查 torch + torchvision 子模块）
    /// 2. torch 没装 → 报错返回（不重装，因为不知道要装哪个版本）
    /// 3. torchvision 残缺 → 自动调 `reinstall_torchvision_torchaudio` 强制重装
    ///    - 重装后再跑一次 probe
    ///    - 仍残缺 → 报错（pytorch.org 上没有匹配 cuda 的 wheel）
    /// 4. 仍失败 → 报错
    ///
    /// **为什么只重装 torchvision，不重装 torch**：
    /// - torch 装错版本时 uv 通常会直接拒绝（cuda 不匹配），不会留下半装状态
    /// - 已知有残缺问题的是 torchvision/torchaudio（0.1.6 yanked + 中断污染）
    /// - torch 主包通常是 PyTorch 团队 release 的稳定版本，不需要自动重装
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

        // 第一次 probe（v3.8 D-L3：含 torchvision 子模块校验）
        let probe = self.probe_with_recovery(venv_path, cancel).await?;

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

        // v3.8 D-L3：torchvision 残缺时自动重装
        if !probe.torchvision.installed {
            let tv_version = probe.torchvision.version.as_deref().unwrap_or("?");
            let tv_err = probe.torchvision.error_type.as_deref().unwrap_or("Unknown");
            let tv_msg = probe.torchvision.error_msg.as_deref().unwrap_or("(无详情)");

            tracing::warn!(
                tv_version,
                tv_err,
                "smoke_test_torch: torchvision 子模块残缺（{}: {}），触发 D-L4 自动重装",
                tv_err,
                tv_msg
            );

            // 从 venv 的 torch 包读 cuda_version（避免硬编码）
            // 走最稳的方式：读 torch/version.py 拿 cuda 后缀
            let cuda_version = self.detect_cuda_version_from_torch(venv_path).unwrap_or(
                crate::config::CudaVersion::Cpu,
            );

            reinstall_torchvision_torchaudio(
                self,
                venv_path,
                &cuda_version,
                cancel,
                None, // smoke_test 阶段不传 line_collector（用户已经看到进度了）
            )
            .await?;

            // 重装后再 probe 一次
            let probe2 = self.probe_with_recovery(venv_path, cancel).await?;
            if !probe2.torchvision.installed {
                let tv_err2 = probe2.torchvision.error_type.as_deref().unwrap_or("Unknown");
                let tv_msg2 = probe2.torchvision.error_msg.as_deref().unwrap_or("(无详情)");
                return Err(EnvError::TorchInstallFailed(format!(
                    "torchvision 自动重装后 smoke test 仍失败：{}\n\
                     第 1 次错误：{}: {}\n\
                     重装后错误：{}: {}\n\n\
                     可能原因：\n\
                     1. pytorch.org 上没有匹配 cu{:?} 的 torchvision wheel\n\
                     2. 磁盘空间不足（torchvision wheel ~50MB）\n\
                     3. 杀毒软件拦截了 _C.pyd 写入\n\n\
                     建议：手动执行 `uv pip install --index-url https://download.pytorch.org/whl/<cu> --force-reinstall torchvision torchaudio`",
                    tv_err2, tv_err, tv_msg, tv_err2, tv_msg2, cuda_version
                )));
            }
            tracing::info!("smoke_test_torch: torchvision 自动重装后 smoke test 通过");
        }

        tracing::info!(
            torch_version = %probe.version.as_deref().unwrap_or("?"),
            tv_version = %probe.torchvision.version.as_deref().unwrap_or("?"),
            "torch smoke test passed"
        );
        Ok(())
    }

    /// v3.8 D-INTEGRATE 辅助：跑 probe 并在 json 解析失败时降级处理
    async fn probe_with_recovery(
        &self,
        venv_path: &Path,
        cancel: &CancellationToken,
    ) -> Result<crate::env_inspector::scripts::TorchProbeResult, EnvError> {
        let json_output = crate::env_inspector::scripts::probe_torch_script(venv_path, cancel)
            .await
            .map_err(|e| EnvError::TorchInstallFailed(format!("smoke test 启动失败: {}", e)))?;
        Ok(crate::env_inspector::scripts::parse_torch_probe(&json_output))
    }

    /// v3.10 新增：探测 venv 中 torch 的 CUDA 可用性
    ///
    /// 用途：仅检查 `torch.cuda.is_available()` 的 bool 结果，不校验 torchvision/torchaudio。
    /// - 比 `smoke_test_torch` 轻量（不触发 D-L3 子模块校验）
    /// - 适合用于"快速判断 venv 中 torch 是否真的能跑 CUDA"
    ///
    /// 用例：`quick_repair_reinstall_consistent` 步骤 5 后调一次，
    /// 若 cuda_available=false 则强制重装 torch。
    pub async fn check_torch_cuda_available(
        &self,
        venv_path: &Path,
        cancel: &CancellationToken,
    ) -> Result<bool, EnvError> {
        let probe = self.probe_with_recovery(venv_path, cancel).await?;
        if !probe.installed {
            return Err(EnvError::TorchInstallFailed(
                "torch 未安装（probe.installed=false），无法探测 cuda_available".to_string(),
            ));
        }
        Ok(probe.cuda_available)
    }

    /// v3.8 D-INTEGRATE 辅助：读 torch/version.py 推 cuda_version
    ///
    /// 用于 smoke_test_torch 不知道 cuda_version 时（避免硬编码）
    /// 返回 None 时调用方用 Cpu 兜底
    fn detect_cuda_version_from_torch(
        &self,
        venv_path: &Path,
    ) -> Option<crate::config::CudaVersion> {
        use crate::config::CudaVersion;
        let version_py = if cfg!(windows) {
            venv_path.join("Lib").join("site-packages").join("torch").join("version.py")
        } else {
            // Unix: <venv>/lib/python3.X/site-packages/torch/version.py
            // 用 read_torch_variant_fast 反推路径麻烦，直接用 read_torch_variant_fast
            return None;
        };
        let content = std::fs::read_to_string(&version_py).ok()?;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("__version__") {
                if let Some(eq_pos) = trimmed.find('=') {
                    let val = trimmed[eq_pos + 1..].trim().trim_matches(|c| c == '\'' || c == '"');
                    if let Some(plus_pos) = val.find('+') {
                        let suffix = &val[plus_pos + 1..];
                        return match suffix {
                            "cu118" => Some(CudaVersion::Cu118),
                            "cu126" => Some(CudaVersion::Cu126),
                            "cu128" => Some(CudaVersion::Cu128),
                            "cu130" => Some(CudaVersion::Cu130),
                            _ => None,
                        };
                    }
                    return Some(CudaVersion::Cpu);
                }
            }
        }
        None
    }

    /// 切换 torch 变体（v3.0 新增，F25）
    ///
    /// v3.6：接 `CancellationToken`
    /// v3.7（F4）：可选 line_collector
    pub async fn install_torch_variant(
        &self,
        venv_path: &Path,
        variant: &crate::python_env::TorchVariant,
        cancel: &CancellationToken,
        line_collector: Option<&std::sync::Arc<crate::common::line_collector::LineCollector>>,
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

        let output = if let Some(collector) = line_collector {
            self.run_cmd_with_log(&args_ref, cancel, collector, "uv:torch-variant")
                .await
                .map_err(|e| EnvError::TorchInstallFailed(e.to_string()))?
        } else {
            self.run_cmd(&args_ref, cancel)
                .await
                .map_err(|e| EnvError::TorchInstallFailed(e.to_string()))?
        };
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

    /// 安装单个 Python 包到 venv（v3.7 新增，用于 transformers 版本切换）
    ///
    /// 执行：`uv pip install --upgrade --python=<venv_python> <package>==<version>`
    ///
    /// - `--upgrade`：确保已装旧版本会被升级/降级到指定版本
    /// - `package==version`：精确版本约束
    ///
    /// v3.7：接 `CancellationToken`，透传给 `run_cmd`
    /// v3.7（F4）：可选 line_collector
    pub async fn install_package(
        &self,
        venv_path: &Path,
        package: &str,
        version: &str,
        cancel: &CancellationToken,
        line_collector: Option<&std::sync::Arc<crate::common::line_collector::LineCollector>>,
    ) -> Result<(), EnvError> {
        let venv_arg = format!("--python={}", venv_python_arg(venv_path));
        let pkg_spec = format!("{}=={}", package, version);

        let args: Vec<&str> = vec![
            "pip",
            "install",
            "--upgrade",
            &venv_arg,
            &pkg_spec,
        ];

        tracing::info!(package, version, ?venv_path, "installing package");

        let output = if let Some(collector) = line_collector {
            self.run_cmd_with_log(&args, cancel, collector, "uv:package")
                .await
                .map_err(|e| EnvError::RequirementsInstallFailed(e.to_string()))?
        } else {
            self.run_cmd(&args, cancel)
                .await
                .map_err(|e| EnvError::RequirementsInstallFailed(e.to_string()))?
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(EnvError::RequirementsInstallFailed(format!(
                "安装 {}=={} 失败: {}",
                package, version, stderr.trim()
            )));
        }

        tracing::info!(package, version, "package installed");
        Ok(())
    }

    /// 安装 requirements.txt
    ///
    /// v3.6：接 `CancellationToken`
    /// v3.7（F4）：可选 line_collector
    /// v3.9：自动过滤 torch 系列行（torch/torchvision/torchaudio 等），避免 requirements.txt
    ///       把 torch 降级到不匹配 cuda 的版本。这是从根本上避免 ComfyUI requirements.txt
    ///       中可能存在的 `torch>=2.0` 等约束污染已正确安装的 torch wheel 的关键。
    /// v3.10：**新增** `pytorch_index` 参数：
    ///   - 传 `Some(url)` → uv 解析时同时查 PyPI 和 pytorch.org，
    ///     torch 系列自动从 pytorch.org 拉（不被 PyPI 的 cpu wheel 覆盖）
    ///   - 传 `None` → 走 PyPI 默认源（保留旧行为，向后兼容）
    ///   - 典型用法：调用方读 `cuda_index_url(config.torch.cuda_version)` 后传入
    ///
    /// v3.10 关键 bug 修复：
    ///   - 不传 pytorch_index 时，uv 解析 transformers 5.x 的 `Requires-Dist: torch>=2.4`
    ///     会触发从 PyPI 拉 torch+cpu，**覆盖** install_torch 装好的 cu128 wheel
    ///   - 修法：始终传 pytorch_index（除 CPU 模式外）
    pub async fn install_requirements(
        &self,
        venv_path: &Path,
        requirements_file: &Path,
        constraints: Option<&Path>,
        pytorch_index: Option<&str>,
        cancel: &CancellationToken,
        line_collector: Option<&std::sync::Arc<crate::common::line_collector::LineCollector>>,
    ) -> Result<(), EnvError> {
        // v3.9 关键修复：先过滤 torch 系列行，避免 ComfyUI requirements.txt 把 torch 降级
        let filtered_req = filter_torch_lines_to_temp(requirements_file, venv_path).await?;
        let req_str = format!("-r={}", filtered_req.to_string_lossy());

        let venv_arg = format!("--python={}", venv_python_arg(venv_path));

        let mut args: Vec<String> = vec![
            "pip".into(),
            "install".into(),
            venv_arg,
            req_str,
        ];
        // v3.10：把 pytorch.org 作为 extra-index-url 加入
        //
        // uv 解析行为：
        // - 默认源（PyPI）仍是主源，所有非 torch 包装这里
        // - pytorch.org 作为补充源，**torch 系列**优先从这里拉
        // - 这样：transformers/av/Pillow 等装 PyPI 源
        //        torch/torchvision/torchaudio 走 pytorch.org（不被覆盖成 +cpu）
        if let Some(url) = pytorch_index {
            args.push("--extra-index-url".into());
            args.push(url.to_string());
        }
        if let Some(c) = constraints {
            args.push("-c".into());
            args.push(c.to_string_lossy().into_owned());
        }
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let output = if let Some(collector) = line_collector {
            self.run_cmd_with_log(&args_ref, cancel, collector, "uv:requirements")
                .await
                .map_err(|e| EnvError::RequirementsInstallFailed(e.to_string()))?
        } else {
            self.run_cmd(&args_ref, cancel)
                .await
                .map_err(|e| EnvError::RequirementsInstallFailed(e.to_string()))?
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(EnvError::RequirementsInstallFailed(stderr.into_owned()));
        }
        tracing::info!(
            ?requirements_file,
            ?constraints,
            ?pytorch_index,
            "requirements installed"
        );
        Ok(())
    }

    /// v1.8 / F36：强制按新 constraints 重装 requirements
    ///
    /// v3.6：接 `CancellationToken`
    /// v3.7（F4）：可选 line_collector
    /// v3.9：自动过滤 torch 系列行（与 `install_requirements` 一致）
    /// v3.10：新增 `pytorch_index` 参数（与 install_requirements 一致）
    pub async fn install_requirements_upgrade(
        &self,
        venv_path: &Path,
        requirements_file: &Path,
        constraints: Option<&Path>,
        pytorch_index: Option<&str>,
        cancel: &CancellationToken,
        line_collector: Option<&std::sync::Arc<crate::common::line_collector::LineCollector>>,
    ) -> Result<(), EnvError> {
        // v3.9 关键修复：先过滤 torch 系列行
        let filtered_req = filter_torch_lines_to_temp(requirements_file, venv_path).await?;
        let req_str = format!("-r={}", filtered_req.to_string_lossy());

        let venv_arg = format!("--python={}", venv_python_arg(venv_path));

        let mut args: Vec<String> = vec![
            "pip".into(),
            "install".into(),
            "--upgrade".into(),
            "--force-reinstall".into(),
            venv_arg,
            req_str,
        ];
        if let Some(url) = pytorch_index {
            args.push("--extra-index-url".into());
            args.push(url.to_string());
        }
        if let Some(c) = constraints {
            args.push("-c".into());
            args.push(c.to_string_lossy().into_owned());
        }
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let output = if let Some(collector) = line_collector {
            self.run_cmd_with_log(&args_ref, cancel, collector, "uv:requirements-upgrade")
                .await
                .map_err(|e| EnvError::RequirementsInstallFailed(e.to_string()))?
        } else {
            self.run_cmd(&args_ref, cancel)
                .await
                .map_err(|e| EnvError::RequirementsInstallFailed(e.to_string()))?
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(EnvError::RequirementsInstallFailed(stderr.into_owned()));
        }
        tracing::info!(
            ?requirements_file,
            ?constraints,
            ?pytorch_index,
            "requirements upgraded with --force-reinstall"
        );
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

    /// 执行 uv 子命令 + 实时日志收集到 LineCollector（v3.7：F4 新增）
    ///
    /// 与 `run_cmd` 区别：stdout/stderr 实时推给 collector，前端 / LogStore 可订阅。
    /// 退出码 / 错误转换逻辑与 `run_cmd` 一致。
    async fn run_cmd_with_log(
        &self,
        args: &[&str],
        cancel: &CancellationToken,
        line_collector: &std::sync::Arc<crate::common::line_collector::LineCollector>,
        source: &str,
    ) -> Result<std::process::Output, std::io::Error> {
        let mut cmd = crate::common::process_util::new_command(&self.uv_binary);
        cmd.args(args);

        crate::common::subprocess::run_with_cancel_and_log(&mut cmd, cancel, line_collector, source)
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

    /// 执行 uv 子命令（v3.8 新增）：根据 line_collector 自动选择带/不带日志的版本
    ///
    /// **作用**：避免在每个调用点都写 `if let Some(collector) = line_collector { run_cmd_with_log } else { run_cmd }` 的重复代码。
    ///
    /// **不返回 EnvError**：保留底层 `std::io::Error`，让调用方根据上下文决定转成哪个 EnvError 变体
    /// （TorchInstallFailed / RequirementsInstallFailed / VerifyFailed 等）。
    async fn run_uv_pip(
        &self,
        args: &[&str],
        cancel: &CancellationToken,
        line_collector: Option<&std::sync::Arc<crate::common::line_collector::LineCollector>>,
        source: &str,
    ) -> Result<std::process::Output, std::io::Error> {
        if let Some(collector) = line_collector {
            self.run_cmd_with_log(args, cancel, collector, source).await
        } else {
            self.run_cmd(args, cancel).await
        }
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
///
/// v3.7：支持 cu118 / cu126 / cu128 / cu130
/// 构造 PyTorch 官方 wheel 索引 URL（v3.9 关键修复：CPU 模式也走 pytorch.org）
///
/// **历史 bug（v3.8 之前）**：
/// - `CudaVersion::Cpu` 时返回 `None`，install_torch 不传 `--index-url`
/// - uv 走 PyPI 默认源，**PyPI 镜像了 pytorch.org 的所有 wheel**（含 cu128）
/// - 步骤 1 装 `torch`：PyPI 上 torch 默认就是 CPU 版 → 装出 `torch+cpu`
/// - 步骤 2 装 `torchvision/torchaudio`：PyPI 提供的 wheel **带 cu128 后缀**
///   （因为 PyPI 镜像了 pytorch.org 的 cu128 目录）
/// - 结果：venv 里 `torch+cpu` + `torchaudio+cu128` + `torchvision+cu128` → **版本混乱**
/// - ComfyUI 启动时 `torch.cuda.is_available() → False`（因为底层 torch 真的没 CUDA）
/// - 报 `AssertionError: Torch not compiled with CUDA enabled`
///
/// **v3.9 修复**：CPU 模式也走 pytorch.org 官方 CPU 源
/// - `https://download.pytorch.org/whl/cpu` 提供与 PyPI **版本号一致**的 CPU wheel
/// - 步骤 1 和步骤 2 都强制走 pytorch.org，保证 torch/torchvision/torchaudio 版本一致
/// - pytorch.org CPU 源不会镜像 cu128 目录，所以装出来的全是 `+cpu` 版本
///
/// v3.10 改为 `pub`：让 `recovery::quick_repair_reinstall_consistent` 等
/// 外部模块可以重用同一源 URL 计算逻辑，避免硬编码。
pub fn cuda_index_url(cuda: &crate::config::CudaVersion) -> Option<String> {
    use crate::config::CudaVersion;
    match cuda {
        // v3.9 修复：CPU 也返回 pytorch.org 官方 CPU 源，避免 PyPI 镜像的 cu128 wheel 混入
        CudaVersion::Cpu => Some("https://download.pytorch.org/whl/cpu".into()),
        CudaVersion::Cu118 => Some("https://download.pytorch.org/whl/cu118".into()),
        CudaVersion::Cu126 => Some("https://download.pytorch.org/whl/cu126".into()),
        CudaVersion::Cu128 => Some("https://download.pytorch.org/whl/cu128".into()),
        CudaVersion::Cu130 => Some("https://download.pytorch.org/whl/cu130".into()),
    }
}

/// v3.8 D-L2：校验 venv 中 torchvision 是否安装完整
///
/// **校验项**：
/// 1. `site-packages/torchvision/__init__.py` 存在
/// 2. `site-packages/torchvision/ops/` 子目录存在（含 roi_align 等 C++ 扩展）
/// 3. `site-packages/torchvision/_C<...>.pyd`（Windows）或 `_C<...>.so`（Linux）存在
/// 4. `__init__.py` 大小 ≥ 1KB（0.1.6 的 `__init__.py` 只有 129 字节，太小；0.22+ 应 ≥ 1KB）
///
/// **为什么需要这层校验**：
/// - `uv pip check` 只看 metadata，不看实际文件
/// - `import torchvision` 成功不代表 torchvision 完整（0.1.6 也能 import，但 `from torchvision.ops import roi_align` 会炸）
/// - `smoke_test_torch`（probe_torch_script）只看 torch 状态，不看 torchvision 子模块
/// - 历史上用户 venv 出现"半新半旧"残缺状态：根目录文件是 0.1.6 旧版，子目录被删过重建但内容还是旧的，ops/io 完全没有
///
/// **返回值**：
/// - `Ok(())` ：torchvision 完整
/// - `Err(reason)` ：torchvision 残缺，reason 是人类可读的描述（不包含 `site-packages` 绝对路径以避免日志噪音）
pub fn verify_torchvision_filesystem(venv_path: &Path) -> Result<(), String> {
    use crate::env_inspector::scripts::venv_python_path;

    let python = venv_python_path(venv_path);
    if !python.exists() {
        return Err("venv python 不存在".into());
    }

    // site-packages 路径（与 python 父目录同级 /lib/pythonX.Y/site-packages 或 /Lib/site-packages）
    // 简化：根据 python 路径推断 site-packages
    let site_packages = if cfg!(windows) {
        // Windows: <venv>/Lib/site-packages
        venv_path.join("Lib").join("site-packages")
    } else {
        // Unix: <venv>/lib/python3.X/site-packages
        // 实际拿 venv 里的 python 路径反推
        let py_str = python.to_string_lossy();
        if let Some(idx) = py_str.rfind("/lib/python") {
            let prefix = &py_str[..idx];
            // 找 /lib/python3.X
            if let Some(rest_idx) = py_str[idx + 1..].find('/') {
                let lib_py = &py_str[idx + 1..idx + 1 + rest_idx];
                PathBuf::from(prefix).join(lib_py).join("site-packages")
            } else {
                PathBuf::from(prefix).join("lib").join("site-packages")
            }
        } else {
            venv_path.join("lib").join("site-packages")
        }
    };

    if !site_packages.exists() {
        return Err(format!("site-packages 不存在: {}", site_packages.display()));
    }

    let torchvision_dir = site_packages.join("torchvision");
    if !torchvision_dir.exists() {
        return Err("torchvision/ 目录不存在".into());
    }

    // 1. __init__.py 必须存在且大小合理
    let init_py = torchvision_dir.join("__init__.py");
    if !init_py.exists() {
        return Err("torchvision/__init__.py 不存在".into());
    }
    let init_size = std::fs::metadata(&init_py).map(|m| m.len()).unwrap_or(0);
    if init_size < 1024 {
        return Err(format!(
            "torchvision/__init__.py 太小 ({} 字节)，疑似远古版（0.1.6 是 129 字节，应 ≥ 1KB）",
            init_size
        ));
    }

    // 2. ops/ 子目录必须存在
    let ops_dir = torchvision_dir.join("ops");
    if !ops_dir.exists() || !ops_dir.is_dir() {
        return Err("torchvision/ops/ 目录不存在（缺 roi_align 等 C++ 扩展）".into());
    }
    // 还要检查 ops/__init__.py 存在
    let ops_init = ops_dir.join("__init__.py");
    if !ops_init.exists() {
        return Err("torchvision/ops/__init__.py 不存在".into());
    }

    // 3. _C<...>.pyd（Windows）或 _C<...>.so（Linux）必须存在
    // pytorch.org 的 wheel 命名：_C.cp311-win_amd64.pyd / _C.cpython-311-x86_64-linux-gnu.so
    // 用通配符扫描
    let _c_found = std::fs::read_dir(&torchvision_dir)
        .ok()
        .and_then(|entries| {
            entries
                .filter_map(|e| e.ok())
                .find(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    // Windows: _C.cp*.pyd 或 _C.cp*-win_amd64.pyd
                    // Linux: _C.cpython-*.so
                    (name.starts_with("_C.") && (name.ends_with(".pyd") || name.ends_with(".so")))
                        || name == "_C.pyd"
                        || name == "_C.so"
                })
                .map(|_| true)
        })
        .unwrap_or(false);
    if !_c_found {
        return Err("torchvision/_C<...>.pyd (或 .so) 不存在（缺 C++ 扩展）".into());
    }

    // 4. io/ 子目录必须存在（read_image 等）
    let io_dir = torchvision_dir.join("io");
    if !io_dir.exists() || !io_dir.is_dir() {
        return Err("torchvision/io/ 目录不存在（缺 read_image 等）".into());
    }

    Ok(())
}

/// v3.8 D-L4：强制重装 torchvision + torchaudio
///
/// **触发条件**：
/// - `install_torch` 装完调 `verify_torchvision_filesystem` 发现残缺
/// - 或 `smoke_test_torch`（probe 含 torchvision 校验）发现 `from torchvision.ops import roi_align` 失败
///
/// **执行命令**：
/// ```text
/// uv pip install --python <venv> --index-url https://download.pytorch.org/whl/<cu>
///   --force-reinstall --no-deps --upgrade
///   torchvision torchaudio
/// ```
///
/// **为什么这些参数**：
/// - `--force-reinstall`：覆盖已有文件（包括半装半新的残缺状态）
/// - `--no-deps`：不解析 torch 依赖约束（避免 uv 退到 PyPI 选到 0.1.6 yanked）
/// - `--upgrade`：强制用最新版本
/// - `--index-url`：严格只在 pytorch.org 上找 wheel
///
/// **重试上限**：本函数只重装 1 次（让 `install_torch` 顶层的二次 verify 兜底）。
/// 如果第 1 次重装后仍残缺 → 报错给用户，不无限重试（避免 uv 装包循环）。
///
/// **v3.9 关键改造**：重装完成后调 `install_torch_extras` 补装 numpy / psutil / six 等。
/// 因为 `--no-deps` 装时不会拉任何依赖，torch 重装后**这些关键运行依赖可能丢失**，
/// 需要立刻补齐，否则后续 ComfyUI 启动会因缺 numpy 触发 "Torch not compiled with CUDA enabled" 误判。
///
/// **为什么接 `uv: &UvRunner` 参数而不是用 `from_path()`**：
/// - launcher 启动时会把 uv 二进制 release 到用户数据目录，**不是** PATH 上的 uv
/// - `from_path()` 会用 `PathBuf::from("uv")`，**不是**绝对路径
/// - 必须用传入的 `uv`（U vRunner 实例的 `uv_binary` 字段才是正确的路径）
pub async fn reinstall_torchvision_torchaudio(
    uv: &UvRunner,
    venv_path: &Path,
    cuda_version: &crate::config::CudaVersion,
    cancel: &CancellationToken,
    line_collector: Option<&std::sync::Arc<crate::common::line_collector::LineCollector>>,
) -> Result<(), EnvError> {
    let venv_arg = format!("--python={}", venv_python_arg(venv_path));
    let index_url = cuda_index_url(cuda_version);

    // v3.9 关键修复：和 install_torch 步骤 2 一样，强制走 pytorch.org
    // - 防止 PyPI 镜像的 cu128 wheel 混入
    // - cuda_index_url 现在永远返回 Some（Cpu → pytorch.org CPU 源）
    let url = match &index_url {
        Some(u) => u.clone(),
        None => {
            return Err(EnvError::TorchInstallFailed(format!(
                "reinstall_torchvision_torchaudio: cuda_index_url 返回 None（cuda_version={:?}），\
                 这是 bug！强制要求 index-url 存在",
                cuda_version
            )));
        }
    };

    let args: Vec<String> = vec![
        "pip".into(),
        "install".into(),
        venv_arg,
        "--force-reinstall".into(),
        "--no-deps".into(),
        "--upgrade".into(),
        "torchvision".into(),
        "torchaudio".into(),
        "--index-url".into(),
        url,
    ];

    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    tracing::warn!(
        ?cuda_version,
        "D-L4 自动重装 torchvision + torchaudio（--force-reinstall --no-deps --upgrade --index-url pytorch.org）"
    );

    // 用传入的 uv 路径（不是 PATH 上的 uv）
    let mut cmd = crate::common::process_util::new_command(uv.binary_path());
    cmd.args(&args_ref)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let output = if let Some(collector) = line_collector {
        crate::common::subprocess::run_with_cancel_and_log(
            &mut cmd,
            cancel,
            collector,
            "uv:torchvision-reinstall",
        )
        .await
    } else {
        crate::common::subprocess::run_with_cancel(&mut cmd, cancel).await
    }
    .map_err(|e| {
        EnvError::TorchInstallFailed(format!("torchvision 自动重装启动失败: {}", e))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(EnvError::TorchInstallFailed(format!(
            "torchvision 自动重装失败: {}",
            stderr
        )));
    }

    // v3.9 关键改造：重装完调 install_torch_extras 补装 numpy / psutil / six 等
    //
    // 原因：reinstall_torchvision_torchaudio 用 `--no-deps`，重装后这些关键运行依赖
    // 可能被"清空"或保留为不兼容版本。必须立刻补装，否则 ComfyUI 启动会因缺 numpy
    // 触发 "Torch not compiled with CUDA enabled" 误判（详见 PR/02-架构/历史事故-2026-07-07.md）。
    //
    // 幂等性：install_torch_extras 用 --upgrade 已是最新版的包 uv 会跳过，无副作用。
    // 失败处理：extras 装失败不视为致命（后续 install_requirements 仍会跑），只 warn。
    tracing::info!("reinstall_torchvision_torchaudio: 重装完成，补装 torch 关键依赖 (numpy/psutil/six/...)");
    if let Err(e) = uv.install_torch_extras(venv_path, cancel, line_collector).await {
        tracing::warn!(
            error = %e,
            "reinstall_torchvision_torchaudio: install_torch_extras 失败（不视为致命，后续 install_requirements 仍会尝试补齐）"
        );
        // 不返回 Err，让 reinstall 主流程视为成功（torchvision/torchaudio 确实装好了）
    }

    Ok(())
}

/// v3.9：需要从 requirements.txt 中过滤掉的 torch 系列包名集合
///
/// **为什么必须过滤**：
/// - 用户已通过 `install_torch` 从 pytorch.org 装了匹配 cuda 的 torch wheel（如 2.11.0+cu128）
/// - ComfyUI requirements.txt 中可能写 `torch>=2.0` 或 `torchvision>=0.15` 这类宽松约束
/// - 如果让这些约束传给 `uv pip install -r`，uv 会尝试"满足约束"：
///   - 宽松约束下，uv 可能从 PyPI 装到无 cuda 后缀的 torch（破坏已装的 cuda 版本）
///   - 或拉到与当前 cuda 不匹配的老版本（让 `torch.cuda.is_available()` 返回 False）
/// - 这就是历史上「一键补装」失败、CUDA 不可用的根本原因之一
///
/// **为什么用集合而不是单点替换**：
/// - torch 生态有很多相关包（torch / torchvision / torchaudio / torchtext / torchao 等）
/// - 不同 ComfyUI 版本的 requirements.txt 写法不一样，必须有兜底
pub const TORCH_SERIES_PACKAGES: &[&str] = &[
    "torch",
    "torchvision",
    "torchaudio",
    "torchtext",
    "torchdata",
    "torchao",
    "torchmetrics",
    "pytorch-lightning",
    "lightning",
];

/// v3.9：把 requirements.txt 中的 torch 系列行过滤掉，写到临时文件，返回临时文件路径
///
/// **输入**：
/// - `requirements_file`：ComfyUI 原版 requirements.txt
/// - `venv_path`：venv 目录（用于存放过滤版临时文件，跟随 venv 重建自动清理）
///
/// **输出**：过滤版的 requirements 文件路径（写到 `<venv>/.requirements-filtered.txt`）
///
/// **过滤规则**：
/// - 注释行（以 `#` 开头）：原样保留
/// - 空行：原样保留
/// - `-r xxx` / `--requirement xxx`：原样保留（子文件可能也含 torch，但 uv 会自己处理）
/// - 形如 `pkg>=1.0` / `pkg==1.0` / `pkg` 的包规范行：pkg 在 `TORCH_SERIES_PACKAGES` 中则过滤掉
/// - 其他行（`-e xxx`、URL 形式等）：原样保留
///
/// **降级策略**：
/// - requirements_file 读取失败 → 返回原文件路径，不做过滤（不影响主流程）
/// - 写入临时文件失败 → 返回原文件路径
///
/// **为什么用 venv 目录而不是 `tempfile::tempdir()`**：
/// - tempfile 的 TempPath/TempDir 跨 await 边界传递很麻烦
/// - 写 venv 目录可以保证 venv 重建时自动清理（无需 RAII 守卫）
/// - venv 目录在 ComfyUI 完整生命周期内是稳定的
pub async fn filter_torch_lines_to_temp(
    requirements_file: &Path,
    venv_path: &Path,
) -> Result<std::path::PathBuf, EnvError> {
    // 1. 读原文件
    let content = match tokio::fs::read_to_string(requirements_file).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                file = %requirements_file.display(),
                error = %e,
                "filter_torch_lines_to_temp: 读取 requirements.txt 失败，跳过过滤（不阻塞主流程）"
            );
            // 降级：返回原文件路径（不过滤）
            return Ok(requirements_file.to_path_buf());
        }
    };

    // 2. 逐行过滤
    let mut filtered_lines: Vec<String> = Vec::with_capacity(content.lines().count());
    let mut removed: Vec<String> = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim_start();
        // 注释 / 空行 / -r 包含文件 → 保留
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
            filtered_lines.push(line.to_string());
            continue;
        }
        // 形如 `pkg>=1.0` / `pkg==1.0` / `pkg` / `pkg~=1.0` / `pkg<=1.0` 等
        // 取第一个非字母数字字符前的包名
        let pkg_name: String = trimmed
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
            .collect::<String>()
            .to_ascii_lowercase();

        if TORCH_SERIES_PACKAGES.iter().any(|p| p.eq_ignore_ascii_case(&pkg_name)) {
            removed.push(line.to_string());
            continue;
        }
        filtered_lines.push(line.to_string());
    }

    // 3. 写临时文件到 venv 目录
    let temp_path = venv_path.join(".requirements-filtered.txt");
    let header = format!(
        "# v3.9 自动生成的过滤版 requirements.txt\n\
         # 已移除以下 torch 系列行（避免 uv 把已正确安装的 cuda 版 torch 降级）：\n\
         # {}\n\n",
        if removed.is_empty() {
            "（无）".to_string()
        } else {
            removed.join("\n# ")
        }
    );
    let body = filtered_lines.join("\n");
    let full = format!("{}{}", header, body);
    tokio::fs::write(&temp_path, full)
        .await
        .map_err(|e| {
            EnvError::RequirementsInstallFailed(format!(
                "写过滤版 requirements 失败: {}",
                e
            ))
        })?;

    if !removed.is_empty() {
        tracing::info!(
            original = %requirements_file.display(),
            filtered = %temp_path.display(),
            removed_count = removed.len(),
            "filter_torch_lines_to_temp: 已过滤 torch 系列行"
        );
    }

    Ok(temp_path)
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
    fn test_cuda_index_url_cu128() {
        let url = cuda_index_url(&CudaVersion::Cu128).unwrap();
        assert!(url.contains("cu128"));
    }

    #[test]
    fn test_cuda_index_url_cu130() {
        let url = cuda_index_url(&CudaVersion::Cu130).unwrap();
        assert!(url.contains("cu130"));
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
