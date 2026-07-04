/**
 * Core Manager Store
 *
 * 设计模式：
 * - **Store (Flux)**：集中管理 ComfyUI 核心仓库状态
 * - **Observer**：监听 `core_checked_out` / `requirements_mismatch` 事件
 *
 * 使用方式：
 * ```ts
 * const coreStore = useCoreStore();
 * await coreStore.subscribe();
 * await coreStore.refresh();
 * ```
 */

import { defineStore } from "pinia";
import { ref, computed } from "vue";
import {
  coreIsCloned,
  coreStatus,
  coreClone,
  coreEnsureCloned,
  coreListTags,
  coreCheckout,
  coreUpdate,
} from "@/api/core";
import { listen, type UnlistenFn } from "@/api";
import type { CoreStatus, GitTag } from "@/api/types";

export const useCoreStore = defineStore("core", () => {
  // ========== State ==========
  const status = ref<CoreStatus | null>(null);
  const tags = ref<GitTag[]>([]);
  const loading = ref(false);
  const error = ref<string | null>(null);
  /** requirements 不匹配标记（来自后端 emit "requirements_mismatch"） */
  const requirementsMismatch = ref(false);
  const unlisteners: UnlistenFn[] = [];

  // ========== Getters ==========
  const isCloned = computed(() => status.value?.is_cloned ?? false);
  const currentVersion = computed(() => status.value?.current_version ?? null);
  const hasUpdates = computed(() => status.value?.has_updates ?? false);

  // ========== Actions ==========

  /** 刷新状态（status + tags） */
  async function refresh() {
    loading.value = true;
    error.value = null;
    try {
      const [s, t] = await Promise.all([coreStatus(), coreListTags()]);
      status.value = s;
      tags.value = t;
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
      throw e;
    } finally {
      loading.value = false;
    }
  }

  /** 克隆 ComfyUI 仓库 */
  async function clone(repoUrl?: string) {
    loading.value = true;
    try {
      await coreClone(repoUrl);
      await refresh();
    } finally {
      loading.value = false;
    }
  }

  /**
   * 确保 ComfyUI 仓库已克隆
   *
   * - 若 comfyui_root 已包含 `.git` → 跳过
   * - 若目录不存在 → 自动 clone 默认仓库
   * - 若目录非空但无 `.git` → 抛错（让前端处理）
   *
   * 注意：refresh 失败和 ensureCloned 失败是两个独立的事，分别 catch，
   * 避免 refresh 错误（如 list_tags 参数问题）覆盖 ensureCloned 的真实错误。
   */
  async function ensureCloned() {
    loading.value = true;
    try {
      await coreEnsureCloned();
    } finally {
      loading.value = false;
    }
    // refresh 单独 try，避免被 catch 块误判为 clone 失败
    try {
      await refresh();
    } catch (e) {
      console.warn("[core] refresh after ensureCloned failed:", e);
    }
  }

  /** 切换版本 */
  async function checkout(ref: string) {
    loading.value = true;
    try {
      await coreCheckout(ref);
      await refresh();
    } finally {
      loading.value = false;
    }
  }

  /** 更新到最新（git pull） */
  async function update() {
    loading.value = true;
    try {
      await coreUpdate();
      await refresh();
    } finally {
      loading.value = false;
    }
  }

  /** 订阅事件 */
  async function subscribe() {
    if (unlisteners.length > 0) return;
    unlisteners.push(
      await listen<{ ref: string }>("core_checked_out", (e) => {
        // 版本切换完成，更新本地状态
        if (status.value) {
          status.value = { ...status.value, current_version: e.payload.ref };
        }
        refresh().catch((err) => console.warn("core refresh failed:", err));
      }),
      await listen<{ mismatch: boolean }>("requirements_mismatch", (e) => {
        requirementsMismatch.value = e.payload.mismatch;
      }),
    );
  }

  function unsubscribe() {
    unlisteners.forEach((un) => un());
    unlisteners.length = 0;
  }

  return {
    // state
    status,
    tags,
    loading,
    error,
    requirementsMismatch,
    // getters
    isCloned,
    currentVersion,
    hasUpdates,
    // actions
    refresh,
    clone,
    ensureCloned,
    checkout,
    update,
    subscribe,
    unsubscribe,
  };
});
