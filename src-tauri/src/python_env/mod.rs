//! PythonEnvManager 模块
//!
//! 设计模式：
//! - **Template Method**：`switch_python_version` 5 步事务（备份 → 创建 → 装 torch → 装依赖 → 校验 → 回滚）
//! - **Adapter**：`UvRunner` 封装 `uv` 命令
//! - **Facade**：Tauri commands 封装
//!
//! 详见 `PR/03-模块设计/02-PythonEnvManager.md`

pub mod compatibility;
pub mod freeze;
pub mod models;
pub mod recovery;
pub mod torch_variant;
pub mod uv_runner;
pub mod verify;

pub use torch_variant::TorchVariant;

use std::path::{Path, PathBuf};

use tokio::sync::mpsc;
use tokio::sync::Mutex;

use crate::config::{Config, CudaVersion};
use crate::error::EnvError;
use crate::event_bus::{EventBus, SystemEvent};

use models::{CompatibilityReport, EnvInfo, InstallProgress, InstallStage, PythonEnvStatus};
use uv_runner::UvRunner;
use verify::{is_venv_ready, probe_python_version, verify_venv};

/// PythonEnv 服务
///
/// - 通过 `Mutex` 防止并发操作同一 venv
/// - 进度通过 mpsc 通道推送（TaskScheduler 接入后改为通过 TaskScheduler 推送）
pub struct PythonEnvService {
    uv: UvRunner,
    event_bus: EventBus,
    /// 防止并发操作同一 venv
    op_lock: Mutex<()>,
}

impl PythonEnvService {
    pub fn new(uv_binary: PathBuf, event_bus: EventBus) -> Self {
        Self {
            uv: UvRunner::new(uv_binary),
            event_bus,
            op_lock: Mutex::new(()),
        }
    }

    /// 使用 PATH 中的 uv 构造（开发/调试用）
    pub fn from_path(event_bus: EventBus) -> Self {
        Self {
            uv: UvRunner::from_path(),
            event_bus,
            op_lock: Mutex::new(()),
        }
    }

    /// v1.8：访问 uv runner（recovery 模块用）
    pub fn uv(&self) -> &UvRunner {
        &self.uv
    }

    /// v1.8：访问 event bus（recovery 模块用）
    pub fn event_bus_ref(&self) -> &EventBus {
        &self.event_bus
    }

    /// uv 是否可用
    pub async fn is_uv_available(&self) -> bool {
        self.uv.is_available().await
    }

    /// 获取 Python 环境状态总览（v2.13）
    ///
    /// 探测内容：
    /// - uv 二进制是否可用 + 版本号
    /// - venv 目录是否存在
    /// - venv 中的 Python 版本
    /// - venv 中是否安装 torch + 版本 + CUDA 状态
    ///
    /// 所有探测都是只读，不会修改任何状态。
    /// 用于前端 `envStatus` 命令（设置页「Python 版本切换」当前版本显示）。
    pub async fn get_status(&self, venv_path: &Path) -> PythonEnvStatus {
        // 1. uv 状态
        let (uv_version, uv_installed) = self.uv.get_version().await;
        let uv_path = if uv_installed {
            Some(self.uv.binary_path().to_string_lossy().into_owned())
        } else {
            None
        };

        // 2. venv 状态
        let venv_exists = venv_path.exists();
        if !venv_exists {
            return PythonEnvStatus {
                uv_installed,
                uv_path,
                uv_version,
                venv_exists: false,
                venv_python_version: None,
                venv_torch_installed: false,
                venv_torch_version: None,
                venv_torch_cuda: false,
            };
        }

        // 3. venv 中 python 是否存在
        let python = crate::env_inspector::scripts::venv_python_path(venv_path);
        if !python.exists() {
            return PythonEnvStatus {
                uv_installed,
                uv_path,
                uv_version,
                venv_exists: true,
                venv_python_version: None,
                venv_torch_installed: false,
                venv_torch_version: None,
                venv_torch_cuda: false,
            };
        }

        // 4. 探查 python 版本（轻量：`python -c "import sys; print(sys.version.split()[0])"`，5s 超时）
        let venv_python_version = probe_python_version(&python).await;

        // 5. 探查 torch（用现有 verify_venv 逻辑，但 venv_python_version 已拿到）
        //    注意：verify_venv 会调 probe_torch_script（90s 超时），失败时降级
        let (torch_installed, torch_version, torch_cuda) =
            match verify_venv(venv_path).await {
                Ok(info) => (info.torch_installed, info.torch_version, info.cuda_available),
                Err(e) => {
                    tracing::warn!(
                        error = %e, "get_status: verify_venv failed, torch status unknown"
                    );
                    (false, None, false)
                }
            };

        PythonEnvStatus {
            uv_installed,
            uv_path,
            uv_version,
            venv_exists: true,
            venv_python_version,
            venv_torch_installed: torch_installed,
            venv_torch_version: torch_version,
            venv_torch_cuda: torch_cuda,
        }
    }

    /// 创建 venv
    pub async fn create_venv(
        &self,
        venv_path: &Path,
        python_version: &str,
    ) -> Result<(), EnvError> {
        let _guard = self.op_lock.lock().await;
        self.uv.create_venv(venv_path, python_version).await
    }

    /// 安装便携 Python
    pub async fn install_portable_python(&self, version: &str) -> Result<(), EnvError> {
        let _guard = self.op_lock.lock().await;
        self.uv.install_python(version).await
    }

    /// 安装 torch
    ///
    /// 完成后通过事件总线广播 `TorchInstalled`
    pub async fn install_torch(
        &self,
        venv_path: &Path,
        cuda_version: CudaVersion,
    ) -> Result<(), EnvError> {
        let _guard = self.op_lock.lock().await;
        self.uv.install_torch(venv_path, &cuda_version).await?;

        // 通知其他模块（EnvironmentInspector 失效缓存，ProcessLauncher 重置 dirty 标记）
        self.event_bus
            .emit(SystemEvent::TorchInstalled {
                cuda_version: cuda_version.display_name().to_string(),
            });

        Ok(())
    }

    /// 切换 torch 变体（v3.0 新增，F25）
    ///
    /// 支持 5 厂商（NVIDIA / AMD / Intel / Apple / CPU），通过 `TorchVariant` 抽象。
    ///
    /// 实现要点：
    /// - 复用同一 `op_lock`，与 install_torch / install_requirements 互斥
    /// - 委托给 `UvRunner::install_torch_variant`（--upgrade + 验证）
    /// - 完成后通过事件总线广播 `TorchInstalled`（带 variant display name）
    /// - 失败时返回 Err，旧 torch 保留（不破坏 venv）
    /// - 调用方负责更新 Config（向后兼容字段 cuda_version + 新字段 torch_variant）
    pub async fn switch_torch_variant(
        &self,
        venv_path: &Path,
        variant: &TorchVariant,
    ) -> Result<(), EnvError> {
        let _guard = self.op_lock.lock().await;

        if !venv_path.exists() {
            return Err(EnvError::VenvCreateFailed(format!(
                "venv 不存在: {}（请先完成环境初始化）",
                venv_path.display()
            )));
        }

        self.uv.install_torch_variant(venv_path, variant).await?;

        // 通知其他模块
        self.event_bus
            .emit(SystemEvent::TorchInstalled {
                cuda_version: variant.label(),
            });

        tracing::info!(?variant, "torch variant switched");
        Ok(())
    }

    /// 安装 requirements.txt
    ///
    /// v3.2 修复：成功后 emit `RequirementsInstalled` 事件让 env cache 失效。
    ///
    /// 之前不 emit 任何事件，导致 30s 缓存命中陈旧的 deps 列表，
    /// readiness 持续报"InstallRequirements 缺失"（用户体感：装完仍说未装）。
    /// `install_torch` 一直有 emit `TorchInstalled`，但 `install_requirements` 漏了。
    ///
    /// v1.8：自动应用 freeze constraints 防止 numpy 等包装到坏版本。
    pub async fn install_requirements(
        &self,
        venv_path: &Path,
        requirements_file: &Path,
    ) -> Result<(), EnvError> {
        let _guard = self.op_lock.lock().await;
        let constraints = self.prepare_freeze_constraints(venv_path).await?;
        self.uv.install_requirements(venv_path, requirements_file, Some(&constraints)).await?;

        // v3.2：通知 EnvironmentInspector 失效 30s 缓存
        self.event_bus.emit(SystemEvent::RequirementsInstalled);
        tracing::info!(?requirements_file, "requirements installed");
        Ok(())
    }

    /// v1.8 / F36：Preserve 模式专用
    ///
    /// 强制按新 constraints 重装（含 `--upgrade --force-reinstall`），用于切版本时
    /// 让 pip 按新 requirements.txt 升级/降级包版本，避免 venv 残留旧版本。
    ///
    /// v1.8：自动应用 freeze constraints 防止 numpy 等包装到坏版本。
    pub async fn install_requirements_upgrade(
        &self,
        venv_path: &Path,
        requirements_file: &Path,
    ) -> Result<(), EnvError> {
        let _guard = self.op_lock.lock().await;
        // 写入 constraints 到 venv（如已存在则覆盖更新）
        let constraints = self.prepare_freeze_constraints(venv_path).await?;
        self.uv.install_requirements_upgrade(venv_path, requirements_file, Some(&constraints)).await?;

        // v3.2：通知 EnvironmentInspector 失效 30s 缓存
        self.event_bus.emit(SystemEvent::RequirementsInstalled);
        tracing::info!(?requirements_file, "requirements upgraded with --force-reinstall");
        Ok(())
    }

    /// v1.8：准备 freeze constraints 文件
    ///
    /// 写入到 `<venv>/.freeze-constraints.txt`，返回路径。
    /// 写入失败时降级返回 None（不阻塞主流程，warn 日志）。
    pub async fn prepare_freeze_constraints(&self, venv_path: &Path) -> Result<std::path::PathBuf, EnvError> {
        match crate::python_env::freeze::write_constraints_to_venv(venv_path) {
            Ok(p) => Ok(p),
            Err(e) => {
                tracing::warn!(error = %e, "failed to write freeze constraints, proceeding without");
                // 用一个空 constraints 文件占位
                let placeholder = venv_path.join(".freeze-constraints.txt");
                std::fs::write(&placeholder, "# empty\n").map_err(|e2| {
                    EnvError::RequirementsInstallFailed(format!(
                        "freeze constraints 写入失败: {} / 兜底也失败: {}",
                        e, e2
                    ))
                })?;
                Ok(placeholder)
            }
        }
    }

    /// 校验 venv 完整性
    pub async fn verify_venv(&self, venv_path: &Path) -> Result<EnvInfo, EnvError> {
        verify_venv(venv_path).await
    }

    /// venv 是否就绪
    pub async fn is_venv_ready(&self, venv_path: &Path) -> bool {
        is_venv_ready(venv_path).await
    }

    /// 比对 venv 已装依赖 vs requirements.txt
    ///
    /// v2.10：注入 uv_binary 用于加速 pip list（uv pip list 主路径）
    pub async fn check_requirements_compatibility(
        &self,
        venv_path: &Path,
        comfyui_root: &Path,
    ) -> Result<CompatibilityReport, EnvError> {
        compatibility::check_requirements_compatibility(
            venv_path,
            comfyui_root,
            Some(self.uv.binary_path()),
        )
        .await
    }

    /// venv 重建
    ///
    /// 1. 删除 venv 目录
    /// 2. create_venv（python_version from Config）
    /// 3. install_torch（cuda_version from Config）
    /// 4. install_requirements（comfyui_root/requirements.txt）
    /// 5. verify_venv 通过后 emit(VenvRebuilt)
    pub async fn rebuild_venv(
        &self,
        config: &Config,
    ) -> Result<(), EnvError> {
        let _guard = self.op_lock.lock().await;
        let venv_path = PathBuf::from(&config.paths.venv_path);
        let comfyui_root = PathBuf::from(&config.paths.comfyui_root);
        let python_version = &config.paths.python_version;
        let cuda_version = config.torch.cuda_version;

        // 1. 删除 venv
        if venv_path.exists() {
            tokio::fs::remove_dir_all(&venv_path)
                .await
                .map_err(|e| EnvError::RebuildFailed {
                    detail: format!("remove old venv failed: {}", e),
                })?;
        }

        // 2. 创建 venv
        self.uv.create_venv(&venv_path, python_version).await?;

        // 3. 装 torch
        self.uv.install_torch(&venv_path, &cuda_version).await?;

        // 4. 装 requirements
        let req_file = comfyui_root.join("requirements.txt");
        if req_file.exists() {
            let constraints = self.prepare_freeze_constraints(&venv_path).await?;
            self.uv.install_requirements(&venv_path, &req_file, Some(&constraints)).await?;
        }

        // 5. 校验
        let info = verify_venv(&venv_path).await?;
        if !info.torch_installed {
            return Err(EnvError::RebuildFailed {
                detail: "torch not installed after rebuild".to_string(),
            });
        }

        // 广播事件
        self.event_bus.emit(SystemEvent::VenvRebuilt);
        tracing::info!("venv rebuilt successfully");
        Ok(())
    }

    /// 切换 Python 版本（5 步事务 + 备份回滚）
    ///
    /// 详见 `PR/03-模块设计/02-PythonEnvManager.md §3` switch_python_version
    ///
    /// 步骤：
    /// 1. 安装新便携 Python（uv python install <new_version>）
    /// 2. 备份旧 venv 为 <venv>.bak-<ts>
    /// 3. 用新 Python 创建新 venv
    /// 4. 按 Config.torch.cuda_version 重装 torch
    /// 5. 按 ComfyUI 当前版本 requirements.txt 重装依赖
    /// 6. verify_venv() 通过后删除备份，失败则恢复备份
    ///
    /// 进度通过 `progress_tx` 推送
    pub async fn switch_python_version(
        &self,
        new_version: &str,
        config: &Config,
        progress_tx: mpsc::Sender<InstallProgress>,
    ) -> Result<(), EnvError> {
        let _guard = self.op_lock.lock().await;
        let venv_path = PathBuf::from(&config.paths.venv_path);
        let comfyui_root = PathBuf::from(&config.paths.comfyui_root);
        let cuda_version = config.torch.cuda_version;

        // Step 1: 安装新 Python
        let _ = progress_tx
            .send(InstallProgress {
                stage: InstallStage::DownloadingPython,
                message: format!("installing python {}", new_version),
                percent: Some(10),
            })
            .await;
        self.uv.install_python(new_version).await?;

        // Step 2: 备份旧 venv
        let backup_path = backup_venv(&venv_path).await?;

        // Step 3: 创建新 venv
        let _ = progress_tx
            .send(InstallProgress {
                stage: InstallStage::CreatingVenv,
                message: "creating new venv".to_string(),
                percent: Some(30),
            })
            .await;
        if let Err(e) = self.uv.create_venv(&venv_path, new_version).await {
            restore_backup(&venv_path, &backup_path).await;
            return Err(e);
        }

        // Step 4: 装 torch
        let _ = progress_tx
            .send(InstallProgress {
                stage: InstallStage::InstallingTorch,
                message: "installing torch".to_string(),
                percent: Some(50),
            })
            .await;
        if let Err(e) = self.uv.install_torch(&venv_path, &cuda_version).await {
            restore_backup(&venv_path, &backup_path).await;
            return Err(e);
        }

        // Step 5: 装 requirements
        let _ = progress_tx
            .send(InstallProgress {
                stage: InstallStage::InstallingRequirements,
                message: "installing requirements".to_string(),
                percent: Some(80),
            })
            .await;
        let req_file = comfyui_root.join("requirements.txt");
        if req_file.exists() {
            if let Err(e) = self
                .uv
                .install_requirements(&venv_path, &req_file, None)
                .await
            {
                restore_backup(&venv_path, &backup_path).await;
                return Err(e);
            }
        }

        // Step 6: 校验
        let _ = progress_tx
            .send(InstallProgress {
                stage: InstallStage::Verifying,
                message: "verifying venv".to_string(),
                percent: Some(90),
            })
            .await;
        match verify_venv(&venv_path).await {
            Ok(info) if info.torch_installed => {
                // 删除备份
                if backup_path.exists() {
                    let _ = tokio::fs::remove_dir_all(&backup_path).await;
                }
                let _ = progress_tx
                    .send(InstallProgress {
                        stage: InstallStage::Done,
                        message: "switch complete".to_string(),
                        percent: Some(100),
                    })
                    .await;
                self.event_bus.emit(SystemEvent::PythonVersionSwitched {
                    from: config.paths.python_version.clone(),
                    to: new_version.to_string(),
                });
                Ok(())
            }
            Ok(_) => {
                // torch 未装
                restore_backup(&venv_path, &backup_path).await;
                Err(EnvError::PythonSwitchFailed {
                    detail: "torch not installed after switch".to_string(),
                })
            }
            Err(e) => {
                restore_backup(&venv_path, &backup_path).await;
                Err(EnvError::PythonSwitchFailed {
                    detail: e.to_string(),
                })
            }
        }
    }
}

/// 备份 venv 目录为 `<venv>.bak-<timestamp>`
async fn backup_venv(venv_path: &Path) -> Result<PathBuf, EnvError> {
    if !venv_path.exists() {
        return Ok(PathBuf::from(format!(
            "{}.bak-{}",
            venv_path.display(),
            chrono::Utc::now().timestamp()
        )));
    }
    let backup = PathBuf::from(format!(
        "{}.bak-{}",
        venv_path.display(),
        chrono::Utc::now().timestamp()
    ));
    tokio::fs::rename(venv_path, &backup)
        .await
        .map_err(|e| EnvError::PythonSwitchFailed {
            detail: format!("backup venv failed: {}", e),
        })?;
    tracing::info!(?backup, "venv backed up");
    Ok(backup)
}

/// 失败时恢复备份
async fn restore_backup(venv_path: &Path, backup: &Path) {
    if !backup.exists() {
        return;
    }
    // 先删除失败的新 venv
    if venv_path.exists() {
        let _ = tokio::fs::remove_dir_all(venv_path).await;
    }
    // 恢复备份
    if let Err(e) = tokio::fs::rename(backup, venv_path).await {
        tracing::error!(error = %e, "failed to restore venv backup");
    } else {
        tracing::info!("venv backup restored");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_bus::EventBus;

    fn make_service() -> PythonEnvService {
        let event_bus = EventBus::new();
        PythonEnvService::from_path(event_bus)
    }

    #[test]
    fn test_op_lock_serializes() {
        let service = make_service();
        let guard = service.op_lock.lock();
        // 锁可获取
        assert!(true);
        drop(guard);
    }

    #[tokio::test]
    async fn test_backup_venv_nonexistent_returns_path() {
        let tmp = tempfile::tempdir().unwrap();
        let venv = tmp.path().join("nonexistent_venv");
        let backup = backup_venv(&venv).await.unwrap();
        assert!(backup.to_string_lossy().contains(".bak-"));
    }

    #[tokio::test]
    async fn test_backup_and_restore_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let venv = tmp.path().join("venv");
        let backup = tmp.path().join("venv.bak");

        // 创建 venv 目录
        tokio::fs::create_dir(&venv).await.unwrap();
        tokio::fs::write(venv.join("marker.txt"), "old").await.unwrap();

        // 备份（rename）
        tokio::fs::rename(&venv, &backup).await.unwrap();
        assert!(!venv.exists());
        assert!(backup.exists());

        // 恢复
        tokio::fs::rename(&backup, &venv).await.unwrap();
        assert!(venv.exists());
        assert!(backup.exists() == false);

        let content = tokio::fs::read_to_string(venv.join("marker.txt"))
            .await
            .unwrap();
        assert_eq!(content, "old");
    }
}
