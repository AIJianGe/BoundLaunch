/**
 * 环境管理 API（合并 EnvironmentInspector + PythonEnvManager）
 *
 * 对应后端 `commands/env_inspector.rs` + `commands/python_env.rs`
 * 详见 `PR/03-模块设计/07-EnvironmentInspector.md` 和 `02-PythonEnvManager.md`
 *
 * F32 改造（v3.3）：
 * - `env_inspect` 返回 `EnvInfo | null`（stale 值，null 表示首次启动无数据）
 * - `env_readiness_check` 返回 `ReadinessCheckResult | null`
 * - 6 个长任务命令返回 `task_id`（字符串），实际进度通过 `task_progress` 事件推送
 * - 删除 `envProbeTorch`（死代码）
 * - 新增 `env_inspect_updated` 事件监听入口
 *
 * 事件：
 * - `env_changed`：环境状态变更（venv 创建 / torch 安装 / 切换 Python 等）
 * - `env_inspect_updated`（F32 新增）：后台 spawn_refresh 完成，payload = 新 EnvInfo
 * - `task_progress` / `task_completed`：长任务进度与完成（由 TaskScheduler 统一推送）
 */

import { invoke } from "./index";
import type {
  EnvInfo,
  PythonEnvStatus,
  CompatibilityResult,
  DependencyInfo,
  ReadinessCheckResult,
  ConflictReport,
  TorchVariant,
  GpuInfo,
  DiagnoseReport,
  RepairAction,
} from "./types";

// ============================================================================
// EnvironmentInspector（F32: P0 探查类 - 立即返回 stale 值 + 后台刷新）
// ============================================================================

/**
 * 环境信息总览（F32 stale 模式）
 *
 * 返回：
 * - `EnvInfo`：立即返回 stale 值（可能是 30s 内的缓存，前端不显示 loading）
 * - `null`：首次启动或 `clear()` 后无数据，前端应显示 loading 并监听
 *   `env_inspect_updated` 事件以接收新快照
 *
 * 后端行为：
 * - 若 cache 过期（stale），立即返回旧值 + 后台 spawn_refresh
 * - 刷新完成后 emit `env_inspect_updated` 事件（payload = 新 EnvInfo）
 *
 * 详见 `PR/03-模块设计/07-EnvironmentInspector.md §14 F32 探查类异步化`
 */
export function envInspect(): Promise<EnvInfo | null> {
  return invoke<EnvInfo | null>("env_inspect");
}

/** 列出 requirements.txt 中的依赖及安装状态 */
export function envListDependencies(): Promise<DependencyInfo[]> {
  return invoke<DependencyInfo[]>("env_list_dependencies");
}

/** 强制清除环境信息缓存（下次 env_inspect 重新检测） */
export function envInvalidateCache(): Promise<void> {
  return invoke<void>("env_invalidate_cache");
}

/**
 * 环境就绪性检查（启动 ComfyUI 前调用）
 *
 * F32 改造：返回 `ReadinessCheckResult | null`
 *
 * - 非 null：基于 stale snapshot 快速构造（≤1ms，不阻塞）
 * - null：首次启动无 snapshot，前端应等 `env_inspect_updated` 事件后重新调用
 *
 * 不修改任何状态（不克隆、不安装），仅做只读检测。
 */
export function envReadinessCheck(): Promise<ReadinessCheckResult | null> {
  return invoke<ReadinessCheckResult | null>("env_readiness_check");
}

// ============================================================================
// PythonEnvManager（F32: P1 长任务类 - 返回 task_id + TaskScheduler 调度）
// ============================================================================

/** uv / venv 状态总览 */
export function envStatus(): Promise<PythonEnvStatus> {
  return invoke<PythonEnvStatus>("env_status");
}

/** uv 是否可用（PATH 查找） */
export function envUvAvailable(): Promise<boolean> {
  return invoke<boolean>("env_uv_available");
}

/**
 * 创建 venv（F32 改造：返回 task_id）
 *
 * @param pythonVersion Python 版本（如 "3.11"）
 * @returns task_id，前端通过 `task_progress` / `task_completed` 事件跟踪进度
 */
export function envCreateVenv(pythonVersion: string): Promise<string> {
  return invoke<string>("env_create_venv", { pythonVersion });
}

/**
 * 安装 torch（F32 改造：返回 task_id）
 *
 * @param cudaVersion CUDA 版本（如 "cu121" / "cpu"）
 * @returns task_id，前端通过 `task_progress` / `task_completed` 事件跟踪进度
 */
export function envInstallTorch(cudaVersion: string): Promise<string> {
  return invoke<string>("env_install_torch", { cudaVersion });
}

/**
 * 安装 ComfyUI requirements.txt 依赖（v2.14；F32 改造：返回 task_id）
 *
 * 幂等：`uv pip install -r requirements.txt` 对已满足的包自动跳过
 * 路径：`<comfyui_root>/requirements.txt`（不存在则后端报错）
 *
 * 用例：
 * - OnboardingPage 阶段 5
 * - 设置页「路径配置」一键补装
 * - 首页「一键补装」按钮（InstallRequirements missing step）
 *
 * @returns task_id
 */
export function envInstallRequirements(): Promise<string> {
  return invoke<string>("env_install_requirements");
}

/**
 * 切换 Python 版本（5 步事务；F32 改造：返回 task_id）
 *
 * 流程：检测 uv → 创建新 venv → 安装 torch → 安装 requirements → 切换。
 * 失败时自动回滚到旧 venv。
 *
 * @returns task_id
 */
export function envSwitchPython(pythonVersion: string): Promise<string> {
  return invoke<string>("env_switch_python", { pythonVersion });
}

/** 检查 torch / CUDA 兼容性 */
export function envCheckCompatibility(): Promise<CompatibilityResult> {
  return invoke<CompatibilityResult>("env_check_compatibility");
}

/** 重建 venv（保留 Python 版本，重装 torch + requirements；F32 改造：返回 task_id） */
export function envRebuildVenv(): Promise<string> {
  return invoke<string>("env_rebuild_venv");
}

/**
 * v3.0 依赖冲突检测
 *
 * 扫描 [comfyui_root]/custom_nodes 下所有节点的 requirements.txt，检测同一 Python 包
 * 被多个自定义节点以不同版本约束引用的情况。
 *
 * **只检测不解决**：返回 ConflictReport，前端展示给用户决策，不阻塞 ComfyUI 启动。
 */
export function envCheckDependencyConflicts(): Promise<ConflictReport> {
  return invoke<ConflictReport>("env_check_dependency_conflicts");
}

// ============================================================================
// v3.0 torch 多厂商支持（F25；F32 改造：返回 task_id）
// ============================================================================

/**
 * 切换 torch 变体（v3.0 新增，F25；F32 改造：返回 task_id）
 *
 * 支持 5 厂商（NVIDIA / AMD / Intel / Apple / CPU）。
 * 切换前会自动停止 ComfyUI（如果运行中），调用方需二次确认用户意图。
 *
 * 流程（action 内部）：停 ComfyUI → uv pip install --upgrade <torch> → 验证 → 更新 Config
 * 失败时返回错误，旧 torch 保留（不破坏 venv）。
 *
 * @returns task_id
 */
export function envChangeTorchVariant(variant: TorchVariant): Promise<string> {
  return invoke<string>("env_change_torch_variant", { variant });
}

// ============================================================================
// v3.0 GPU 自动检测 + 智能推荐（F25）
// ============================================================================

/**
 * 跨平台 GPU 检测（带 5 分钟缓存）
 *
 * @param forceRefresh 强制刷新（清除缓存重新检测），默认 false
 *
 * 返回所有检测到的 GPU 列表（含厂商 / 型号 / VRAM / 驱动版本 / CUDA 版本）。
 * 失败或无 GPU 时返回空数组。
 *
 * 检测实现：
 * - NVIDIA: `nvidia-smi`（Windows / Linux）
 * - AMD: `rocm-smi`（Linux）/ WMI（Windows）
 * - Intel: WMI（Windows）/ `sycl-ls`（Linux）
 * - Apple: `system_profiler`（macOS）
 */
export function systemDetectGpus(forceRefresh = false): Promise<GpuInfo[]> {
  return invoke<GpuInfo[]>("system_detect_gpus", { forceRefresh });
}

/** 清除 GPU 检测缓存 */
export function systemClearGpuCache(): Promise<void> {
  return invoke<void>("system_clear_gpu_cache");
}

/**
 * 智能推荐 torch 变体
 *
 * 策略：NVIDIA > AMD(Linux/Windows) > Intel > Apple > CPU
 *
 * 同时返回 `GpuInfo[]`（含检测到的 GPU 列表，方便 UI 展示）。
 */
export function systemRecommendTorch(): Promise<TorchVariant> {
  return invoke<TorchVariant>("system_recommend_torch");
}

// ============================================================================
// v1.8 / F36-Phase2：环境诊断 + 修复（对应 src-tauri/src/python_env/recovery.rs）
// ============================================================================

/**
 * 环境诊断（v1.8 / F36-Phase2）
 *
 * 不会修改任何后端状态，纯只读探测。
 * 返回 `DiagnoseReport`：
 * - `venv_exists` / `torch_import_ok` / `torch_version`
 * - `issues[]`：诊断出的所有问题（按严重度排序）
 * - `suggested_action`：综合建议（最严重 action）
 * - `suggested_reason`：建议原因（用户可读）
 *
 * 用法：用户在「环境检查」页看到 torch 未安装时，点「诊断」按钮触发。
 */
export function envDiagnose(): Promise<DiagnoseReport> {
  return invoke<DiagnoseReport>("env_diagnose");
}

/**
 * 环境修复（v1.8 / F36-Phase2）
 *
 * F32 改造：返回 task_id，实际执行由 TaskScheduler 调度。
 * 进度通过 `task_progress` 事件推送，完成通过 `task_completed` 事件通知。
 *
 * @param action 修复动作（建议从 `envDiagnose` 拿到的 `suggested_action` 传入）
 * @returns task_id
 */
export function envRepair(action: RepairAction): Promise<string> {
  return invoke<string>("env_repair", { action });
}
