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

/**
 * 确保 ComfyUI 仓库已克隆
 *
 * 行为：
 * - 若 `comfyui_root/.git` 已存在 → 直接返回
 * - 若目录不存在或为空 → 自动 clone 默认仓库
 * - 若目录非空且无 `.git` → 返回错误（前端提示用户处理）
 *
 * 用法：OnboardingPage / LaunchPage 在需要 ComfyUI 源码时调用。
 */
export function coreEnsureCloned(): Promise<void> {
  return invoke<void>("core_ensure_cloned");
}

/**
 * 列出远程 tag（用于版本切换）
 *
 * @param force 是否强制刷新（跳过内存缓存）；默认 false（用缓存）
 */
export function coreListTags(force: boolean = false): Promise<GitTag[]> {
  return invoke<GitTag[]>("core_list_tags", { force });
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
