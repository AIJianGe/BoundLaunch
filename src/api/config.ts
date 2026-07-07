/**
 * Config 模块 API
 *
 * 对应后端 `commands/config.rs`
 * 详见 `PR/03-模块设计/01-Config.md §3 接口签名`
 */

import { invoke } from "./index";
import type { Config, ConfigUpdate } from "./types";

/** 读取完整 Config */
export function configGet(): Promise<Config> {
  return invoke<Config>("config_get");
}

/** 获取 launcher 工作目录（ComfyUI 根目录默认值） */
export function configLauncherWorkingDir(): Promise<string> {
  return invoke<string>("config_launcher_working_dir");
}

/**
 * v1.8 / F38：获取 portable 模式数据位置信息
 *
 * 字段分为两组：
 * - 推导值（从 paths::* 算出来，不依赖 Config）
 * - 实际值（从 Config.paths 读出来，用户可能改过）
 */
export interface DataLocationInfo {
  /** 当前生效的数据目录（dev → <project_root>/data/，prod → <exe_dir>/data/） */
  data_dir: string;
  /** 当前生效的 cache 目录（与 data/ 平行） */
  cache_dir: string;
  /** ComfyUI 根目录默认值（推导） */
  comfyui_root_default: string;
  /** venv 默认路径（推导） */
  venv_path_default: string;
  /** portable 模式基础目录（dev → 项目根 / prod → exe 旁 / null = 不可解析） */
  portable_base_dir: string | null;
  /** 模式来源 */
  mode: "env" | "portable" | "legacy";
  /** 模式说明（人类可读） */
  mode_description: string;
  /** 可执行文件绝对路径 */
  executable_path: string | null;

  // ===== v1.8 / F38：Config 里的实际值 =====
  /** Config.paths.comfyui_root 实际值 */
  comfyui_root_actual: string | null;
  /** Config.paths.venv_path 实际值 */
  venv_path_actual: string | null;
  /** Config.paths.models_path 实际值（null = 未配置） */
  models_path_actual: string | null;
  /** comfyui_root 是否在默认位置 */
  comfyui_root_is_default: boolean;
  /** venv_path 是否在默认位置 */
  venv_path_is_default: boolean;
}

export function configDataLocation(): Promise<DataLocationInfo> {
  return invoke<DataLocationInfo>("config_data_location");
}

/**
 * 部分更新 Config（深合并语义）
 *
 * 仅传需要更新的字段，未传字段保留原值。
 * 成功后后端会 emit "config_changed" 事件（含完整 Config）。
 */
export function configUpdate(update: ConfigUpdate): Promise<Config> {
  return invoke<Config>("config_update", { update });
}

/** 重置 Config 到默认值（保留 paths） */
export function configReset(): Promise<Config> {
  return invoke<Config>("config_reset");
}
