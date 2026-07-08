//! EnvironmentInspector 模块 - 环境探查器
//!
//! 设计模式：
//! - **Cache-Aside + stale 模式（F32）**：30s TTL 内存缓存；`invalidate()` 不删值，
//!   仅标记 `stale=true`，invoke 立即返回旧值，后台 spawn 刷新
//! - **Strategy**：不同 OS 不同 python 二进制路径
//! - **Facade**：Tauri commands 封装内部实现
//!
//! F32 改造（v3.3）：
//! - 探查类命令（`env_inspect` / `env_readiness_check`）改为「立即返回 stale 值 +
//!   后台 spawn 刷新 + emit `env_inspect_updated` 事件」模式
//! - 不再阻塞前端 5-90s 等待 `import torch`
//! - 详见 `PR/03-模块设计/07-EnvironmentInspector.md §14 F32 探查类异步化`
//!
//! 详见 `PR/03-模块设计/07-EnvironmentInspector.md`

pub mod cache;
pub mod dependency_conflict;
pub mod deps;
pub mod gpu;
pub mod models;
pub mod readiness;
pub mod scripts;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::Utc;
use parking_lot::RwLock;
use serde_json::Value;
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

use crate::error::EnvError;
use crate::event_bus::{EventBus, SystemEvent};

use cache::EnvCache;
use deps::build_dependency_list;
use gpu::detect_gpu;
use models::{DependencyInfo, EnvInfo, EnvSnapshot, GpuInfo, TorchInfo, TorchvisionInfo};
use scripts::{probe_torch_script, run_pip_list, venv_python_path};

/// EnvironmentInspector 服务
///
/// - 30s TTL 内存缓存（F32 stale 模式）
/// - 事件总线订阅（TorchInstalled / VenvRebuilt / CoreVersionSwitched / RequirementsInstalled）
///   主动失效（仅标记 stale，不删值）
/// - v2.10：持有 uv_binary 用于加速 `pip list` 探查
/// - F32：新增 `refreshing` CAS 防并发刷新 + `snapshot_cache` 存最后一次 EnvSnapshot +
///   `app_handle` 用于 emit `env_inspect_updated` 事件
#[derive(Clone)]
pub struct EnvironmentInspectorService {
    cache: EnvCache,
    event_bus: EventBus,
    /// uv binary 路径（用于 uv pip list 加速依赖探查）
    ///
    /// - `Some(path)`：生产环境从 lib.rs 注入 uv sidecar 路径
    /// - `None`：测试场景或 uv 不可用，run_pip_list 会直接走 fallback 路径
    uv_binary: Option<PathBuf>,
    /// F32 新增：后台刷新进行中标志（CAS 防并发刷新）
    ///
    /// - `false`：空闲
    /// - `true`：已有 spawn_refresh 任务在跑，跳过新请求
    refreshing: Arc<AtomicBool>,
    /// F32 新增：Tauri AppHandle，用于 emit `env_inspect_updated` 事件给前端
    ///
    /// - `Some(handle)`：生产环境从 lib.rs 注入
    /// - `None`：测试场景，spawn_refresh 完成后只更新 cache 不 emit 前端事件
    app_handle: Option<AppHandle>,
    /// F32 新增：最后一次返回给前端的 EnvSnapshot（独立于 EnvInfo cache）
    ///
    /// 设计原因：`inspect_snapshot` 调用 `python_version_from_torch_probe`（5s 超时），
    /// 不适合 ≤100ms 快速返回路径。改为：
    /// - invoke 立即返回 snapshot_cache 中的 stale 值
    /// - spawn_refresh 调完整 inspect_snapshot 后更新 snapshot_cache
    snapshot_cache: Arc<RwLock<Option<EnvSnapshot>>>,
    /// v3.6 新增：后台刷新任务的 CancellationToken
    ///
    /// `spawn_refresh` 每次创建新 token：
    /// - 先取消旧的（取消上次未完成的刷新任务）
    /// - 把新 token 存入此字段，传给 `inspect_snapshot` → 各子探针
    /// - AppExiting 时取消该 token，确保退出时不再有子进程残留
    refresh_cancel: Arc<RwLock<Option<CancellationToken>>>,
}

impl EnvironmentInspectorService {
    /// 生产构造：注入 uv binary 路径（向后兼容，app_handle=None）
    ///
    /// uv_binary 通常来自 `uv_sidecar::ensure_released()` 返回的 sidecar 路径。
    ///
    /// 注：此构造函数不注入 AppHandle，spawn_refresh 完成后不会 emit `env_inspect_updated`
    /// 前端事件。生产环境应改用 [`Self::new_with_app`]。
    pub fn new(event_bus: EventBus, uv_binary: PathBuf) -> Self {
        Self::build(event_bus, Some(uv_binary), None)
    }

    /// F32 新增：生产构造（注入 AppHandle，支持 emit `env_inspect_updated`）
    ///
    /// 推荐在 `lib.rs` setup hook 中使用此构造函数。
    pub fn new_with_app(event_bus: EventBus, uv_binary: PathBuf, app_handle: AppHandle) -> Self {
        Self::build(event_bus, Some(uv_binary), Some(app_handle))
    }

    /// F32 新增：生产构造（uv 可选 + 注入 AppHandle）
    ///
    /// 用于 `lib.rs` 中 uv sidecar 不可用但仍需 emit 事件的场景：
    /// - `uv_binary = None`：run_pip_list 走 python -m pip fallback
    /// - `app_handle = Some(handle)`：spawn_refresh 完成后仍会 emit `env_inspect_updated`
    pub fn new_with_app_optional(
        event_bus: EventBus,
        uv_binary: Option<PathBuf>,
        app_handle: AppHandle,
    ) -> Self {
        Self::build(event_bus, uv_binary, Some(app_handle))
    }

    /// 测试构造：不注入 uv binary（run_pip_list 走 python -m pip fallback）
    pub fn new_for_test(event_bus: EventBus) -> Self {
        Self::build(event_bus, None, None)
    }

    /// 内部统一构造逻辑
    fn build(
        event_bus: EventBus,
        uv_binary: Option<PathBuf>,
        app_handle: Option<AppHandle>,
    ) -> Self {
        let service = Self {
            cache: EnvCache::new(),
            event_bus,
            uv_binary,
            refreshing: Arc::new(AtomicBool::new(false)),
            app_handle,
            snapshot_cache: Arc::new(RwLock::new(None)),
            refresh_cancel: Arc::new(RwLock::new(None)),
        };
        service.spawn_event_listener();
        service
    }

    /// 启动事件总线订阅（监听外部模块变更，主动失效缓存）
    ///
    /// F32 改造：
    /// - TorchInstalled / RequirementsInstalled / VenvRebuilt / CoreVersionSwitched
    ///   → `cache.invalidate()`（仅标记 stale，保留旧值）
    /// - AppExiting → `cache.clear()`（完全清空，避免退出后还持有大对象）
    fn spawn_event_listener(&self) {
        let mut rx = self.event_bus.subscribe();
        let cache = self.cache.clone(); // EnvCache 内部用 Arc，可安全 clone
        let refresh_cancel = self.refresh_cancel.clone();
        tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
                match event {
                    SystemEvent::TorchInstalled { .. }
                    | SystemEvent::RequirementsInstalled
                    | SystemEvent::VenvRebuilt
                    | SystemEvent::CoreVersionSwitched { .. } => {
                        cache.invalidate();
                        tracing::debug!(?event, "env cache invalidated by event");
                    }
                    SystemEvent::AppExiting { .. } => {
                        // 取消正在进行的后台刷新任务，避免退出后子进程残留
                        if let Some(token) = refresh_cancel.write().take() {
                            token.cancel();
                            tracing::debug!("refresh_cancel triggered on app exit");
                        }
                        cache.clear();
                        tracing::debug!(?event, "env cache cleared on app exit");
                    }
                    _ => {}
                }
            }
        });
    }

    /// 完整环境探查
    ///
    /// 5 个子任务并行执行，部分失败返回部分结果
    ///
    /// v3.6：接 `CancellationToken`，透传给 `probe_torch` 和 `inspect_dependencies`。
    /// 缓存命中时立即返回（不检查 cancel）。
    pub async fn inspect_all(
        &self,
        venv_path: &Path,
        comfyui_root: &Path,
        cancel: &CancellationToken,
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
        //    v3.6：cancel 透传给所有子探针（含 detect_gpu，nvidia-smi 可卡住）
        let (torch, dependencies, gpu) = tokio::join!(
            self.probe_torch(venv_path, cancel),
            self.inspect_dependencies(venv_path, comfyui_root, cancel),
            detect_gpu(cancel),
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
        cancel: &CancellationToken,
    ) -> Result<EnvSnapshot, EnvError> {
        // 1. 复用 inspect_all 拿嵌套数据（走 30s 缓存）
        let inner = self.inspect_all(venv_path, comfyui_root, cancel).await?;

        // 2. python 路径
        let python_path = venv_python_path(venv_path);
        let python_path_str = python_path.to_string_lossy().into_owned();

        // 3. comfyui_cloned 探测（v2.13 临时实现：检查 .git 目录）
        let comfyui_cloned = is_comfyui_cloned(comfyui_root);

        // 4. python_version 提取
        //    优先用 probe_torch 输出的 platform.release（已有数据，无额外探测成本）
        //    备选用 static "unknown"
        let python_version = python_version_from_torch_probe(venv_path, cancel).await
            .unwrap_or_else(|| "unknown".to_string());

        // 5. 扁平化
        Ok(EnvSnapshot {
            python_path: python_path_str,
            venv_path: venv_path.to_string_lossy().into_owned(),
            comfyui_root: comfyui_root.to_string_lossy().into_owned(),
            python_version,
            torch_installed: inner.torch.installed,
            torch_version: inner.torch.version,
            torchvision_installed: inner.torch.torchvision.installed,
            torchvision_ops_available: inner.torch.torchvision.ops_available,
            torchvision_io_available: inner.torch.torchvision.io_available,
            cuda_available: inner.torch.cuda_available,
            cuda_version: inner.torch.cuda_version,
            gpu_name: gpu_display_name(&inner.gpu),
            comfyui_cloned,
            dependencies: inner.dependencies,
            last_updated: inner.inspected_at,
        })
    }

    /// F32 新增：探查或返回 stale 值（不阻塞，立即返回）
    ///
    /// 用于 P0 探查类命令（`env_inspect` / `env_readiness_check`）的快速返回路径：
    ///
    /// 1. **读 `snapshot_cache`**：返回最后一次完整 `inspect_snapshot` 的结果（不检查新鲜度）。
    ///    即使 `cache.invalidate()` 已标记 stale，仍返回旧值，前端不显示 loading。
    /// 2. **触发后台刷新**：若 `cache.needs_refresh(venv_path)` 为 true，
    ///    调 `spawn_refresh` 异步刷新（不等待，立即返回当前 stale 值）。
    /// 3. **首次启动**：`snapshot_cache` 为空 → 返回 `None`，
    ///    前端可选择显示 loading 或调用 `env_invalidate_cache` 后等待第一次 `env_inspect_updated` 事件。
    ///
    /// 返回值：
    /// - `Some(snapshot)`：立即返回给前端（可能是 stale 值）
    /// - `None`：首次启动或 `clear()` 后无数据，前端应等待 `env_inspect_updated` 事件
    ///
    /// 副作用：可能触发 `spawn_refresh`，刷新完成后会：
    /// - 更新 `snapshot_cache`
    /// - emit Tauri 2 Event `env_inspect_updated`（payload = 新 EnvSnapshot）
    /// - emit 后端 `EnvInspectUpdated` 事件
    pub fn inspect_or_cached(
        &self,
        venv_path: &Path,
        comfyui_root: &Path,
    ) -> Option<EnvSnapshot> {
        // 1. 读 snapshot_cache（不阻塞）
        let snapshot = self.snapshot_cache.read().clone();

        // 2. 检查是否需要后台刷新
        if self.cache.needs_refresh(venv_path) {
            tracing::debug!(
                has_stale = snapshot.is_some(),
                "env cache needs refresh, spawning background refresh"
            );
            self.spawn_refresh(venv_path.to_path_buf(), comfyui_root.to_path_buf());
        } else {
            tracing::debug!("env cache fresh, no refresh needed");
        }

        snapshot
    }

    /// F32 新增：后台 spawn 刷新任务（CAS 防并发）
    ///
    /// **不阻塞调用方**：CAS 失败说明已有刷新在跑，直接返回。
    /// CAS 成功则 spawn tokio task 执行完整 `inspect_snapshot`，完成后：
    /// 1. 更新 `snapshot_cache`
    /// 2. emit Tauri 2 Event `env_inspect_updated`（前端通过 `listen('env_inspect_updated')` 接收）
    /// 3. emit 后端 `EnvInspectUpdated` 事件（其他 Service 联动）
    /// 4. 重置 `refreshing = false`
    ///
    /// 失败容错：
    /// - `inspect_snapshot` 失败 → 记录 warn，保留旧 `snapshot_cache` 不动
    /// - emit 失败 → 记录 warn（不阻塞流程）
    ///
    /// 注意：本方法接受 `PathBuf`（非 `&Path`），因为需要 'static 生命周期 spawn task。
    fn spawn_refresh(&self, venv_path: PathBuf, comfyui_root: PathBuf) {
        // 1. CAS：如果已在刷新，直接返回
        if self
            .refreshing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            tracing::debug!("env refresh already in progress, skip spawn");
            return;
        }

        // 2. 创建新的 CancellationToken，取消旧的
        //    - 取消旧 token 会让旧 spawn_refresh 内的所有子探针尽快返回 Cancelled
        //    - 取旧 token 取出后置 None，避免重复取消
        let new_cancel = CancellationToken::new();
        if let Some(old) = self.refresh_cancel.write().replace(new_cancel.clone()) {
            tracing::debug!("cancelling previous env refresh task");
            old.cancel();
        }

        // 3. clone self 进 task（EnvironmentInspectorService 已 derive Clone）
        let service = self.clone();
        let venv = venv_path.clone();
        let comfyui = comfyui_root.clone();

        tokio::spawn(async move {
            tracing::info!("env background refresh started");

            let result = service.inspect_snapshot(&venv, &comfyui, &new_cancel).await;

            match result {
                Ok(snapshot) => {
                    // 1. 更新 snapshot_cache
                    *service.snapshot_cache.write() = Some(snapshot.clone());

                    // 2. emit Tauri 2 Event 给前端
                    if let Some(app) = &service.app_handle {
                        if let Err(e) = app.emit("env_inspect_updated", &snapshot) {
                            tracing::warn!(error = %e, "emit env_inspect_updated failed");
                        } else {
                            tracing::debug!("emitted env_inspect_updated event to frontend");
                        }
                    } else {
                        tracing::debug!("app_handle None (test mode), skip emit env_inspect_updated");
                    }

                    // 3. emit 后端 SystemEvent（其他 Service 联动）
                    service.event_bus.emit(SystemEvent::EnvInspectUpdated);

                    tracing::info!(
                        torch_installed = snapshot.torch_installed,
                        "env background refresh complete"
                    );
                }
                Err(e) => {
                    // 失败：保留旧 snapshot_cache 不动，前端继续用 stale 值
                    tracing::warn!(
                        error = %e,
                        "env background refresh failed, keeping stale snapshot"
                    );
                }
            }

            // 4. 重置 refreshing（无论成功失败）
            service.refreshing.store(false, Ordering::SeqCst);
        });
    }

    /// 探查 venv 中的 torch
    ///
    /// v3.6：接 `CancellationToken`，透传给 `probe_torch_script`
    pub async fn probe_torch(
        &self,
        venv_path: &Path,
        cancel: &CancellationToken,
    ) -> Result<TorchInfo, EnvError> {
        if !venv_exists(venv_path) {
            return Err(EnvError::VerifyFailed(format!(
                "venv not found at {}",
                venv_path.display()
            )));
        }

        let stdout = probe_torch_script(venv_path, cancel).await?;
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

        let torchvision = parsed.get("torchvision").map(|tv| TorchvisionInfo {
            installed: tv.get("installed").and_then(|v| v.as_bool()).unwrap_or(false),
            version: tv.get("version").and_then(|v| v.as_str()).map(String::from),
            ops_available: tv.get("ops_available").and_then(|v| v.as_bool()).unwrap_or(false),
            io_available: tv.get("io_available").and_then(|v| v.as_bool()).unwrap_or(false),
        }).unwrap_or_default();

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
            torchvision,
        })
    }

    /// 列出关键依赖
    ///
    /// v3.6：接 `CancellationToken`，透传给 `run_pip_list`
    pub async fn inspect_dependencies(
        &self,
        venv_path: &Path,
        comfyui_root: &Path,
        cancel: &CancellationToken,
    ) -> Result<Vec<DependencyInfo>, EnvError> {
        if !venv_exists(venv_path) {
            return Err(EnvError::VerifyFailed(format!(
                "venv not found at {}",
                venv_path.display()
            )));
        }

        // 1. pip list（v2.10：uv pip list 主路径 + python -m pip fallback）
        let pip_json = run_pip_list(venv_path, self.uv_binary.as_deref(), cancel).await?;
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
    ///
    /// v3.6：接 `CancellationToken`，nvidia-smi 卡住时可取消
    pub async fn detect_gpu(&self, cancel: &CancellationToken) -> GpuInfo {
        detect_gpu(cancel).await
    }

    /// 验证 venv 是否可用（python + torch + 关键包存在性）
    ///
    /// v3.6：接 `CancellationToken`，透传给 `probe_torch`
    pub async fn verify_venv(
        &self,
        venv_path: &Path,
        cancel: &CancellationToken,
    ) -> Result<bool, EnvError> {
        if !venv_exists(venv_path) {
            return Ok(false);
        }
        let torch = self.probe_torch(venv_path, cancel).await?;
        Ok(torch.installed)
    }

    /// 主动失效缓存
    ///
    /// F32 改造：仅调 `cache.invalidate()`（标记 stale=true，保留旧值）。
    /// 下次 `inspect_or_cached` 调用时会自动触发 `spawn_refresh` 后台刷新。
    ///
    /// 不再同步触发刷新，避免阻塞调用方（如 `env_change_torch_variant` 命令）。
    /// 前端在收到 `task_completed` 事件后会主动调 `env_inspect`，触发刷新。
    pub fn invalidate_cache(&self) {
        self.cache.invalidate();
    }

    /// 同步强制刷新 env snapshot 并写回 `snapshot_cache`
    ///
    /// **v3.x 新增**：解决"切换版本后首次启动看到未就绪"的问题。
    ///
    /// 用例：版本切换、venv 重建、requirements 装完等"大动作"完成后，
    /// **同步**跑一次完整探查并立即把结果写回 `snapshot_cache` + emit 前端事件，
    /// 避免用户切到首页时还看到 5-30s 前的旧 `snapshot_cache`。
    ///
    /// 与 `spawn_refresh` 的区别：
    /// - `spawn_refresh` 是**异步**的（`tokio::spawn`），不阻塞调用方，5-30s 后才更新
    /// - `force_snapshot_update` 是**同步**的（`await`），阻塞调用方 5-30s，但调用方
    ///   返回时 `snapshot_cache` 已经是新的
    ///
    /// 行为：
    /// 1. await `inspect_snapshot`（内部走 `inspect_all`，会更新 30s TTL `cache`）
    /// 2. 把结果写入 `snapshot_cache`（同步路径，不依赖 `spawn_refresh`）
    /// 3. emit Tauri 2 Event `env_inspect_updated` 给前端
    /// 4. emit `EnvInspectUpdated` SystemEvent 给后端其他 Service
    ///
    /// 错误处理：
    /// - `inspect_snapshot` 失败 → `snapshot_cache` 保持旧值，返回 `Err`
    /// - emit 失败 → 记录 warn，不影响返回值
    ///
    /// 注意：本方法接受 `CancellationToken` 用于透传给子探针，
    /// 切版本时建议传 `switcher` 的 cancel_token，便于级联取消。
    pub async fn force_snapshot_update(
        &self,
        venv_path: &Path,
        comfyui_root: &Path,
        cancel: &CancellationToken,
    ) -> Result<EnvSnapshot, EnvError> {
        tracing::info!("force snapshot update started");
        let snapshot = self.inspect_snapshot(venv_path, comfyui_root, cancel).await?;

        // 1. 写回 snapshot_cache
        *self.snapshot_cache.write() = Some(snapshot.clone());

        // 2. emit Tauri 2 Event 给前端
        if let Some(app) = &self.app_handle {
            if let Err(e) = app.emit("env_inspect_updated", &snapshot) {
                tracing::warn!(error = %e, "emit env_inspect_updated failed");
            } else {
                tracing::debug!("emitted env_inspect_updated event to frontend (force)");
            }
        } else {
            tracing::debug!("app_handle None (test mode), skip emit env_inspect_updated");
        }

        // 3. emit 后端 SystemEvent（其他 Service 联动）
        self.event_bus.emit(SystemEvent::EnvInspectUpdated);

        tracing::info!(
            torch_installed = snapshot.torch_installed,
            "force snapshot update complete"
        );
        Ok(snapshot)
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
///
/// v3.6：接 `CancellationToken`，透传给 `probe_python_version`
async fn python_version_from_torch_probe(
    venv_path: &Path,
    cancel: &CancellationToken,
) -> Option<String> {
    let python = venv_python_path(venv_path);
    if !python.exists() {
        return None;
    }
    crate::python_env::verify::probe_python_version(&python, cancel).await
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
        let cancel = CancellationToken::new();
        let result = service.probe_torch(Path::new("/nonexistent/venv"), &cancel).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_inspect_dependencies_missing_venv() {
        let service = make_service();
        let cancel = CancellationToken::new();
        let result = service
            .inspect_dependencies(
                Path::new("/nonexistent/venv"),
                Path::new("/nonexistent/comfyui"),
                &cancel,
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_verify_venv_missing_returns_false() {
        let service = make_service();
        let cancel = CancellationToken::new();
        let result = service.verify_venv(Path::new("/nonexistent/venv"), &cancel).await;
        assert_eq!(result.unwrap(), false);
    }

    #[tokio::test]
    async fn test_detect_gpu_returns_value() {
        let service = make_service();
        let cancel = CancellationToken::new();
        let _gpu = service.detect_gpu(&cancel).await;
        // 不验证具体值，只验证不 panic
    }
}
