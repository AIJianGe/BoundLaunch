/**
 * CoreManager 模块 API
 *
 * 对应后端 `commands/core_manager.rs`
 * 详见 `PR/03-模块设计/03-CoreManager.md`
 */

import { invoke } from "./index";
import type { CoreStatus, GitTag } from "./types";

/** 是否已克隆 */
export function coreIsCloned(): Promise<boolean> {
  return invoke<boolean>("core_is_cloned");
}

/** 状态总览（当前版本 / 是否有更新 / 克隆状态） */
export function coreStatus(): Promise<CoreStatus> {
  return invoke<CoreStatus>("core_status");
}

/**
 * 克隆 ComfyUI 仓库
 *
 * @param repoUrl 仓库 URL（默认 https://github.com/comfyanonymous/ComfyUI.git）
 */
export function coreClone(repoUrl?: string): Promise<void> {
  return invoke<void>("core_clone", repoUrl ? { repoUrl } : undefined);
}

/** 列出远程 tag（用于版本切换） */
export function coreListTags(): Promise<GitTag[]> {
  return invoke<GitTag[]>("core_list_tags");
}

/**
 * 切换到指定版本（tag / commit / branch）
 *
 * @param ref Git 引用（如 "v0.6.0" / "main" / commit SHA）
 */
export function coreCheckout(ref: string): Promise<void> {
  return invoke<void>("core_checkout", { ref });
}

/** 拉取最新代码（git pull） */
export function coreUpdate(): Promise<void> {
  return invoke<void>("core_update");
}
