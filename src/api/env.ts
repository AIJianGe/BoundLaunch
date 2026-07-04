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
