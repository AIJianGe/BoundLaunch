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

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::Value;

use crate::error::EnvError;
use crate::event_bus::{EventBus, SystemEvent};

use cache::EnvCache;
use deps::build_dependency_list;
use gpu::detect_gpu;
use models::{DependencyInfo, EnvInfo, EnvSnapshot, GpuInfo, TorchInfo};
use scripts::{probe_torch_script, run_pip_list, venv_python_path};

/// EnvironmentInspector 服务
///
/// - 30s TTL 内存缓存
/// - 事件总线订阅（TorchInstalled / VenvRebuilt / CoreVersionSwitched）主动失效
/// - v2.10：持有 uv_binary 用于加速 `pip list` 探查（uv pip list 主路径 + python -m pip fallback）
pub struct EnvironmentInspectorService {
    cache: EnvCache,
    event_bus: EventBus,
    /// uv binary 路径（用于 uv pip list 加速依赖探查）
    ///
    /// - `Some(path)`：生产环境从 lib.rs 注入 uv sidecar 路径
    /// - `None`：测试场景或 uv 不可用，run_pip_list 会直接走 fallback 路径
    uv_binary: Option<PathBuf>,
}

impl EnvironmentInspectorService {
    /// 生产构造：注入 uv binary 路径
    ///
    /// uv_binary 通常来自 `uv_sidecar::ensure_released()` 返回的 sidecar 路径。
    pub fn new(event_bus: EventBus, uv_binary: PathBuf) -> Self {
        let service = Self {
            cache: EnvCache::new(),
            event_bus,
            uv_binary: Some(uv_binary),
        };
        service.spawn_event_listener();
        service
    }

    /// 测试构造：不注入 uv binary（run_pip_list 走 python -m pip fallback）
    pub fn new_for_test(event_bus: EventBus) -> Self {
        let service = Self {
            cache: EnvCache::new(),
            event_bus,
            uv_binary: None,
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

    /// 扁平环境快照（v2.13 前端 `env_inspect` 命令专用）
    ///
    /// 与 `inspect_all` 的区别：
    /// - `inspect_all` 返回嵌套 `EnvInfo`（模块内部用）
    /// - `inspect_snapshot` 返回扁平 `EnvSnapshot`（前端用，字段与前端 `EnvInfo` 类型完全对齐）
    ///
    /// 缓存逻辑：
    /// - 先调 `inspect_all` 走原有 30s TTL 缓存
    /// - 再把嵌套结果扁平化 + 补全 `comfyui_cloned` / `python_path` 等新字段
    ///
    /// comfyui_cloned 探测：检查 comfyui_root 目录是否存在 + 是否为合法 git 仓库
    /// （含 `.git` 目录）。Phase 7+ 接入 CoreManager 后可替换为 `core_manager.is_cloned()`。
    pub async fn inspect_snapshot(
        &self,
        venv_path: &Path,
        comfyui_root: &Path,
    ) -> Result<EnvSnapshot, EnvError> {
        // 1. 复用 inspect_all 拿嵌套数据（走 30s 缓存）
        let inner = self.inspect_all(venv_path, comfyui_root).await?;

        // 2. python 路径
        let python_path = venv_python_path(venv_path);
        let python_path_str = python_path.to_string_lossy().into_owned();

        // 3. comfyui_cloned 探测（v2.13 临时实现：检查 .git 目录）
        let comfyui_cloned = is_comfyui_cloned(comfyui_root);

        // 4. python_version 提取
        //    优先用 probe_torch 输出的 platform.release（已有数据，无额外探测成本）
        //    备选用 static "unknown"
        let python_version = python_version_from_torch_probe(venv_path).await
            .unwrap_or_else(|| "unknown".to_string());

        // 5. 扁平化
        Ok(EnvSnapshot {
            python_path: python_path_str,
            venv_path: venv_path.to_string_lossy().into_owned(),
            comfyui_root: comfyui_root.to_string_lossy().into_owned(),
            python_version,
            torch_installed: inner.torch.installed,
            torch_version: inner.torch.version,
            cuda_available: inner.torch.cuda_available,
            cuda_version: inner.torch.cuda_version,
            gpu_name: gpu_display_name(&inner.gpu),
            comfyui_cloned,
            dependencies: inner.dependencies,
            last_updated: inner.inspected_at,
        })
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

        // 1. pip list（v2.10：uv pip list 主路径 + python -m pip fallback）
        let pip_json = run_pip_list(venv_path, self.uv_binary.as_deref()).await?;
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

/// v2.13：检查 ComfyUI 目录是否是已克隆的 git 仓库
///
/// 临时实现：检查 `<comfyui_root>/.git` 目录是否存在。
/// Phase 7+ 接入 CoreManager 后应替换为 `core_manager.is_cloned(comfyui_root)`。
fn is_comfyui_cloned(comfyui_root: &Path) -> bool {
    if !comfyui_root.exists() || !comfyui_root.is_dir() {
        return false;
    }
    comfyui_root.join(".git").exists()
}

/// v2.13：把 GpuInfo 枚举扁平化为字符串（给前端展示用）
fn gpu_display_name(gpu: &GpuInfo) -> Option<String> {
    match gpu {
        GpuInfo::Nvidia { name, .. } => Some(name.clone()),
        GpuInfo::Amd { name } => Some(name.clone()),
        GpuInfo::Intel { name } => Some(name.clone()),
        GpuInfo::CpuOnly { cpu_model } => Some(cpu_model.clone()),
        GpuInfo::Unknown => None,
    }
}

/// v2.13：从 probe_torch_script 输出中提取 Python 版本
///
/// 复用现有 `PROBE_TORCH_SCRIPT` 中的 `platform.release` 字段（其实是 OS release，
/// 不是 python 版本）。真正的 Python 版本需要单独探测，但为避免重复跑脚本，
/// 我们重新跑一个轻量探测（5s 超时）。
async fn python_version_from_torch_probe(venv_path: &Path) -> Option<String> {
    let python = venv_python_path(venv_path);
    if !python.exists() {
        return None;
    }
    crate::python_env::verify::probe_python_version(&python).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_service() -> EnvironmentInspectorService {
        let event_bus = EventBus::new();
        EnvironmentInspectorService::new_for_test(event_bus)
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
