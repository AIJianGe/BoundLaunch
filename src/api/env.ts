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
  RepairAction,
  HardwareChangeReport,
  DriverCompatReport,
  VenvTorchConsistency,
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

/**
 * 列出 requirements.txt 中的依赖及安装状态
 *
 * v3.6：返回 `DependencyInfo[] | null`
 * - 非 null：从 EnvSnapshot 缓存提取（立即返回，不阻塞）
 * - null：首次启动或 cache 为空，前端应等 `env_inspect_updated` 事件后重新调用
 */
export function envListDependencies(): Promise<DependencyInfo[] | null> {
  return invoke<DependencyInfo[] | null>("env_list_dependencies");
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
 * @param cudaVersion CUDA 版本（如 "cu128" / "cpu"）
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

/**
 * v3.10 驱动兼容性深度检查
 *
 * 一站式入口：检测 GPU + 推荐变体 + 驱动兼容性检查。
 *
 * 触发场景：
 * - 用户点击「智能推荐」按钮
 * - 启动时检测到 torch 与驱动不匹配
 *
 * @returns 驱动兼容性报告（含 `severity` / `recommended_variant` / `notes`）
 */
export function systemCheckDriverCompat(): Promise<DriverCompatReport> {
  return invoke<DriverCompatReport>("system_check_driver_compat");
}

// ============================================================================
// v3.x Phase 3：硬件变化检测（对应 src-tauri/src/commands/system.rs）
// ============================================================================

/**
 * 硬件变化检测（启动时调用）
 *
 * 用途：
 * - 用户升级驱动 / 换显卡 / 跨机器复制环境时主动探测
 * - 检测到变化返回 `has_change: true` + `recommended_action`
 *
 * 推荐动作（前端弹窗决策）：
 * - `reinstall_torch`：GPU 列表变化 → 强烈建议重装
 * - `optional`：仅驱动版本变化 → 可选
 * - `no_action`：无变化
 *
 * 前端在启动 5-10s 后调用，避免阻塞主流程。
 */
export function systemCheckHardwareChange(): Promise<HardwareChangeReport> {
  return invoke<HardwareChangeReport>("system_check_hardware_change");
}

// ============================================================================
// v3.x Phase 6：venv torch 一致性检测
// ============================================================================

/**
 * 检查 venv 里的 torch 与配置 cuda_version 是否一致
 *
 * 用 `python -c "import torch; print(torch.version.cuda, torch.cuda.is_available())"`
 * 拿 venv 里 torch 的实际 CUDA 版本，对比配置。
 *
 * @param venvPython venv 里的 python 路径
 * @param configuredCuda 当前配置的 CUDA 版本（"cu118" | "cu126" | "cu128" | "cu130"）
 * @returns
 * - `null` → 探测失败（无 python / 无 torch / python 启动失败）
 * - `{ ok: true }` → 一致
 * - `{ ok: false, reason }` → 不一致
 */
export function systemCheckVenvTorchConsistency(
  venvPython: string,
  configuredCuda: string,
): Promise<VenvTorchConsistency> {
  return invoke<VenvTorchConsistency>("system_check_venv_torch_consistency", {
    venvPython,
    configuredCuda,
  });
}

// ============================================================================
// v1.8 / F36-Phase2：环境诊断 + 修复（对应 src-tauri/src/python_env/recovery.rs）
// ============================================================================

/**
 * 环境诊断（v1.8 / F36-Phase2）
 *
 * v3.6 改造：从同步命令改为 TaskScheduler 任务，返回 task_id。
 * - 进度通过 `task_progress` 事件推送（10% 开始 / 50% torch 探针 / 100% 完成）
 * - 完成后通过 `task_completed` 事件返回 `DiagnoseReport`（在 `payload` 字段）
 * - 用户可通过 `task_cancel` 命令取消（torch 探针可能耗时 90s）
 *
 * 诊断完成后后端自动 emit `RequirementsInstalled` → env cache 失效 →
 * `env_inspect_updated` 事件，前端 store 自动拿到最新 EnvSnapshot。
 *
 * @returns task_id，前端通过 `waitForTask` 等待完成并从 payload 取 DiagnoseReport
 */
export function envDiagnose(): Promise<string> {
  return invoke<string>("env_diagnose");
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

// ============================================================================
// v3.10：torch 一致性诊断（mismatch 检测）
// ============================================================================

/**
 * torch 一致性建议
 */
export type TorchConsistencyRecommendation =
  | "no_action"
  | "reinstall_torch"
  | "rebuild_venv"
  | "check_driver";

/**
 * torch 一致性报告
 */
export interface TorchConsistencyReport {
  /** 是否完全一致 */
  consistent: boolean;
  /** Config 期望的 cuda_version（如 "cu128" / "cpu"） */
  config_cuda_version: string;
  /** venv 中实际 torch 版本（如 "2.12.1+cpu"） */
  venv_torch_version: string | null;
  /** venv 中 torch.cuda.is_available() */
  venv_cuda_available: boolean;
  /** 人类可读的问题列表 */
  issues: string[];
  /** 修复建议 */
  recommendation: TorchConsistencyRecommendation;
}

/**
 * 检测 venv 中的 torch 状态是否与 Config 一致（v3.10 新增）
 *
 * 调用方式：
 * - 启动 ComfyUI 前自动调用
 * - 「一键补装」流程中显式调用
 * - 「关键依赖」页面用户点「诊断」时调用
 */
export function envCheckTorchConsistency(): Promise<TorchConsistencyReport> {
  return invoke<TorchConsistencyReport>("env_check_torch_consistency");
}

/**
 * 强制一致重装 torch（v3.10 新增）
 *
 * 用 `--force-reinstall --no-deps --index-url pytorch.org` 强制覆盖重装
 * torch/torchvision/torchaudio，**不破坏 venv 中的其他包**。
 *
 * 返回 task_id，由 TaskScheduler 异步执行。
 */
export function envRepairConsistent(cudaVersion: string): Promise<string> {
  return invoke<string>("env_repair_consistent", { cudaVersion });
}

// ============================================================================
// v3.7：transformers 版本切换
// ============================================================================

/**
 * 列出所有可用 transformers 版本（v3.7 新增）
 *
 * 从后端 `TransformersVersionIndex` 获取版本列表（三层缓存：L1 内存 → L2 文件 → L3 fallback）。
 * 同步返回，不阻塞。
 *
 * 版本列表降序排列（最新在前），包含 4.x 和 5.x。
 * 前端应将 5.x 标记为「实验」（破坏性 API 变更）。
 *
 * 后端启动时会自动后台拉取 PyPI 最新版本列表，前端也可监听
 * `transformers_versions_updated` 事件以接收刷新后的列表。
 */
export function envListTransformersVersions(): Promise<string[]> {
  return invoke<string[]>("env_list_transformers_versions");
}

/**
 * 切换 transformers 版本（v3.7 新增）
 *
 * F32 模式：返回 task_id，实际执行由 TaskScheduler 调度。
 * - 进度通过 `task_progress` 事件推送（10% 开始 / 50% uv pip install / 90% 校验 / 100% 完成）
 * - 完成后通过 `task_completed` 事件通知前端
 * - 完成后自动 emit `RequirementsInstalled` 让 env cache 失效
 *
 * @param version 目标版本号（如 "4.57.3" 或 "5.13.0"）
 * @returns task_id，前端通过 `waitForTask` 等待完成
 */
export function envSwitchTransformers(version: string): Promise<string> {
  return invoke<string>("env_switch_transformers", { version });
}

/**
 * 恢复默认 transformers 版本（v3.7 新增）
 *
 * 按 ComfyUI `requirements.txt` 中的 `transformers>=X.Y.Z` 约束，
 * 从版本列表选满足约束的最新 4.x 版本（排除 5.x 破坏性变更）切换。
 *
 * F32 模式：返回 task_id，`task_completed` 事件的 payload 包含选定的版本号：`{ "version": "4.57.3" }`
 *
 * @returns task_id
 */
export function envRestoreTransformersDefault(): Promise<string> {
  return invoke<string>("env_restore_transformers_default");
}
