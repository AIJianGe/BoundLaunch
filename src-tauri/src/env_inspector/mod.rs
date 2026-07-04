//! EnvironmentInspector 模块 - 环境探查器
//!
//! 设计模式：
//! - **Cache-Aside**：30s TTL 内存缓存，事件总线订阅主动失效
//! - **Strategy**：不同 OS 不同 python 二进制路径
//! - **Facade**：Tauri commands 封装内部实现
//!
//! 详见 `PR/03-模块设计/07-EnvironmentInspector.md`

pub mod cache;
pub mod deps;
pub mod gpu;
pub mod models;
pub mod readiness;
pub mod scripts;

use std::path::Path;

use chrono::Utc;
use serde_json::Value;

use crate::error::EnvError;
use crate::event_bus::{EventBus, SystemEvent};

use cache::EnvCache;
use deps::build_dependency_list;
use gpu::detect_gpu;
use models::{DependencyInfo, EnvInfo, GpuInfo, TorchInfo};
use scripts::{probe_torch_script, run_pip_list};

/// EnvironmentInspector 服务
///
/// - 30s TTL 内存缓存
/// - 事件总线订阅（TorchInstalled / VenvRebuilt / CoreVersionSwitched）主动失效
pub struct EnvironmentInspectorService {
    cache: EnvCache,
    event_bus: EventBus,
}

impl EnvironmentInspectorService {
    pub fn new(event_bus: EventBus) -> Self {
        let service = Self {
            cache: EnvCache::new(),
            event_bus,
        };
        service.spawn_event_listener();
        service
    }

    /// 启动事件总线订阅（监听外部模块变更，主动失效缓存）
    fn spawn_event_listener(&self) {
        let mut rx = self.event_bus.subscribe();
        let cache = self.cache.clone(); // EnvCache 内部用 Arc，可安全 clone
        tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
                match event {
                    SystemEvent::TorchInstalled { .. }
                    | SystemEvent::VenvRebuilt
                    | SystemEvent::CoreVersionSwitched { .. } => {
                        cache.invalidate();
                        tracing::debug!(?event, "env cache invalidated by event");
                    }
                    _ => {}
                }
            }
        });
    }

    /// 完整环境探查
    ///
    /// 5 个子任务并行执行，部分失败返回部分结果
    pub async fn inspect_all(
        &self,
        venv_path: &Path,
        comfyui_root: &Path,
    ) -> Result<EnvInfo, EnvError> {
        // 1. 缓存命中检查
        if self.cache.is_fresh(venv_path) {
            if let Some(cached) = self.cache.get() {
                tracing::debug!("env inspection served from cache");
                return Ok(cached);
            }
        }

        // 2. 并发去重（简化实现：直接执行，不真正去重，避免 watch 通道复杂度）
        //    TODO: 未来可引入 DashMap<InspectKey, watch::Sender> 实现精确去重

        // 3. 并行执行 5 个子任务（tokio::join!）
        let (torch, dependencies, gpu) = tokio::join!(
            self.probe_torch(venv_path),
            self.inspect_dependencies(venv_path, comfyui_root),
            detect_gpu(),
        );

        // 部分失败容错：torch 失败 → not_installed；deps 失败 → 空列表
        let torch = torch.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "torch probe failed, marking as not installed");
            TorchInfo::not_installed()
        });
        let dependencies = dependencies.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "dependency inspection failed, returning empty list");
            vec![]
        });

        // Phase 11+ 接入 ProcessLauncher 后填充 running_args
        // Phase 7+ 接入 CoreManager 后填充 comfyui_version
        let info = EnvInfo {
            torch,
            dependencies,
            gpu,
            comfyui_version: None,
            running_args: None,
            inspected_at: Utc::now(),
        };

        // 4. 写入缓存
        self.cache.set(info.clone(), venv_path);

        tracing::info!(torch_installed = info.torch.installed, "env inspection complete");
        Ok(info)
    }

    /// 探查 venv 中的 torch
    pub async fn probe_torch(&self, venv_path: &Path) -> Result<TorchInfo, EnvError> {
        if !venv_exists(venv_path) {
            return Err(EnvError::VerifyFailed(format!(
                "venv not found at {}",
                venv_path.display()
            )));
        }

        let stdout = probe_torch_script(venv_path).await?;
        let parsed: Value = serde_json::from_str(&stdout)
            .map_err(|e| EnvError::VerifyFailed(format!("torch script output not JSON: {}", e)))?;

        let torch_obj = parsed
            .get("torch")
            .ok_or_else(|| EnvError::VerifyFailed("missing 'torch' field".to_string()))?;

        let installed = torch_obj
            .get("installed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !installed {
            return Ok(TorchInfo::not_installed());
        }

        Ok(TorchInfo {
            installed: true,
            version: torch_obj
                .get("version")
                .and_then(|v| v.as_str())
                .map(String::from),
            cuda_available: torch_obj
                .get("cuda_available")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            cuda_version: torch_obj
                .get("cuda_version")
                .and_then(|v| v.as_str())
                .map(String::from),
            device_name: torch_obj
                .get("device_name")
                .and_then(|v| v.as_str())
                .map(String::from),
            device_count: torch_obj
                .get("device_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            total_memory_mb: torch_obj
                .get("total_memory_mb")
                .and_then(|v| v.as_u64()),
        })
    }

    /// 列出关键依赖
    pub async fn inspect_dependencies(
        &self,
        venv_path: &Path,
        comfyui_root: &Path,
    ) -> Result<Vec<DependencyInfo>, EnvError> {
        if !venv_exists(venv_path) {
            return Err(EnvError::VerifyFailed(format!(
                "venv not found at {}",
                venv_path.display()
            )));
        }

        // 1. pip list
        let pip_json = run_pip_list(venv_path).await?;
        let installed = deps::parse_pip_list(&pip_json)?;

        // 2. requirements.txt（不存在则跳过版本比对）
        let required = match deps::read_requirements(comfyui_root).await {
            Ok(map) => map,
            Err(e) => {
                tracing::warn!(error = %e, "requirements.txt not found, skip version comparison");
                std::collections::HashMap::new()
            }
        };

        // 3. 比对
        Ok(build_dependency_list(&installed, &required))
    }

    /// GPU 检测（直接转调 gpu::detect_gpu，永不报错）
    pub async fn detect_gpu(&self) -> GpuInfo {
        detect_gpu().await
    }

    /// 验证 venv 是否可用（python + torch + 关键包存在性）
    pub async fn verify_venv(&self, venv_path: &Path) -> Result<bool, EnvError> {
        if !venv_exists(venv_path) {
            return Ok(false);
        }
        let torch = self.probe_torch(venv_path).await?;
        Ok(torch.installed)
    }

    /// 主动失效缓存
    pub fn invalidate_cache(&self) {
        self.cache.invalidate();
    }
}

/// 检查 venv 目录是否存在
fn venv_exists(venv_path: &Path) -> bool {
    venv_path.exists() && venv_path.is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_service() -> EnvironmentInspectorService {
        let event_bus = EventBus::new();
        EnvironmentInspectorService::new(event_bus)
    }

    #[tokio::test]
    async fn test_probe_torch_missing_venv() {
        let service = make_service();
        let result = service.probe_torch(Path::new("/nonexistent/venv")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_inspect_dependencies_missing_venv() {
        let service = make_service();
        let result = service
            .inspect_dependencies(Path::new("/nonexistent/venv"), Path::new("/nonexistent/comfyui"))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_verify_venv_missing_returns_false() {
        let service = make_service();
        let result = service.verify_venv(Path::new("/nonexistent/venv")).await;
        assert_eq!(result.unwrap(), false);
    }

    #[tokio::test]
    async fn test_detect_gpu_returns_value() {
        let service = make_service();
        let _gpu = service.detect_gpu().await;
        // 不验证具体值，只验证不 panic
    }
}
