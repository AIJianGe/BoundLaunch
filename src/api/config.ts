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
