/**
 * 环境管理 API（合并 EnvironmentInspector + PythonEnvManager）
 *
 * 对应后端 `commands/env_inspector.rs` + `commands/python_env.rs`
 * 详见 `PR/03-模块设计/07-EnvironmentInspector.md` 和 `02-PythonEnvManager.md`
 *
 * 事件：
 * - `env_changed`：环境状态变更（venv 创建 / torch 安装 / 切换 Python 等）
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
} from "./types";

// ============================================================================
// EnvironmentInspector
// ============================================================================

/**
 * 环境信息总览（含 30s 缓存）
 *
 * 返回：Python 路径 / 版本 / torch / CUDA / GPU / ComfyUI 克隆状态 / 依赖列表
 */
export function envInspect(): Promise<EnvInfo> {
  return invoke<EnvInfo>("env_inspect");
}

/** 探测 torch 安装状态（触发真实的 torch.cuda.is_available() 调用） */
export function envProbeTorch(): Promise<{ torch_installed: boolean; cuda_available: boolean }> {
  return invoke<{ torch_installed: boolean; cuda_available: boolean }>("env_probe_torch");
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
 * 返回 `ReadinessCheckResult`：
 * - `ready = true`：环境就绪，可直接调 `process_start`
 * - `ready = false`：缺失步骤在 `missing_steps` 中（按顺序），前端可依次引导/自动补齐
 *
 * 不修改任何状态（不克隆、不安装），仅做只读检测。
 */
export function envReadinessCheck(): Promise<ReadinessCheckResult> {
  return invoke<ReadinessCheckResult>("env_readiness_check");
}

// ============================================================================
// PythonEnvManager
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
 * 创建 venv
 *
 * @param pythonVersion Python 版本（如 "3.11"）
 */
export function envCreateVenv(pythonVersion: string): Promise<void> {
  return invoke<void>("env_create_venv", { pythonVersion });
}

/**
 * 安装 torch
 *
 * @param cudaVersion CUDA 版本（如 "cu121" / "cpu"）
 */
export function envInstallTorch(cudaVersion: string): Promise<void> {
  return invoke<void>("env_install_torch", { cudaVersion });
}

/**
 * 安装 ComfyUI requirements.txt 依赖（v2.14）
 *
 * 幂等：`uv pip install -r requirements.txt` 对已满足的包自动跳过
 * 路径：`<comfyui_root>/requirements.txt`（不存在则后端报错）
 *
 * 用例：
 * - OnboardingPage 阶段 5
 * - 设置页「路径配置」一键补装
 * - 首页「一键补装」按钮（InstallRequirements missing step）
 */
export function envInstallRequirements(): Promise<void> {
  return invoke<void>("env_install_requirements");
}

/**
 * 切换 Python 版本（5 步事务）
 *
 * 流程：检测 uv → 创建新 venv → 安装 torch → 安装 requirements → 切换。
 * 失败时自动回滚到旧 venv。
 */
export function envSwitchPython(pythonVersion: string): Promise<void> {
  return invoke<void>("env_switch_python", { pythonVersion });
}

/** 检查 torch / CUDA 兼容性 */
export function envCheckCompatibility(): Promise<CompatibilityResult> {
  return invoke<CompatibilityResult>("env_check_compatibility");
}

/** 重建 venv（保留 Python 版本，重装 torch + requirements） */
export function envRebuildVenv(): Promise<void> {
  return invoke<void>("env_rebuild_venv");
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
// v3.0 torch 多厂商支持（F25）
// ============================================================================

/**
 * 切换 torch 变体（v3.0 新增，F25）
 *
 * 支持 5 厂商（NVIDIA / AMD / Intel / Apple / CPU）。
 * 切换前会自动停止 ComfyUI（如果运行中），调用方需二次确认用户意图。
 *
 * 流程：停 ComfyUI → uv pip install --upgrade <torch> → 验证 → 更新 Config
 * 失败时返回错误，旧 torch 保留（不破坏 venv）。
 */
export function envChangeTorchVariant(variant: TorchVariant): Promise<void> {
  return invoke<void>("env_change_torch_variant", { variant });
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
