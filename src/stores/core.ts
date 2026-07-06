/**
 * Core Manager Store
 *
 * 设计模式：
 * - **Store (Flux)**：集中管理 ComfyUI 核心仓库状态
 * - **Observer**：监听 `core_version_switched` / `requirements_mismatch` 事件
 *
 * 使用方式：
 * ```ts
 * const coreStore = useCoreStore();
 * await coreStore.subscribe();
 * await coreStore.refresh();
 * ```
 *
 * v3.1 / F26 新增：
 * - stableTags / prereleaseTags（按 SemVer 分类的 tag 列表）
 * - switchVersion（完整版本切换，11 步流程 + 全部回滚）
 * - checkSwitchPrerequisites（前置条件检查）
 * - ensureModelsLink（models 软链接管理）
 */

import { defineStore } from "pinia";
import { ref, computed } from "vue";
import {
  coreStatus,
  coreClone,
  coreEnsureCloned,
  coreListTagsClassified,
  coreCheckSwitchPrerequisites,
  coreSwitchVersionWithMode,
  coreEnsureModelsLink,
  coreCheckout,
  coreUpdate,
  coreGetRepoUrl,
  coreListBackups,
  coreSetRepoUrl,
  coreRestoreBackup,
} from "@/api/core";
import { listen, type UnlistenFn } from "@/api";
import type {
  CoreStatus,
  TagInfo,
  ClassifiedTags,
  SwitchPrerequisites,
  CheckoutResult,
  BackupInfo,
  SwitchRepoResult,
} from "@/api/types";

export const useCoreStore = defineStore("core", () => {
  // ========== State ==========
  const status = ref<CoreStatus | null>(null);
  /** 所有 tag（兼容旧 API，新代码请用 stableTags / prereleaseTags） */
  const tags = ref<TagInfo[]>([]);
  /** 稳定版 tag 列表（v3.1 / F26 决策 7：NTab 第一类） */
  const stableTags = ref<TagInfo[]>([]);
  /** 预发布版 tag 列表（v3.1 / F26 决策 7：NTab 第二类） */
  const prereleaseTags = ref<TagInfo[]>([]);
  /** 切换版本前置条件（v3.1 / F26 决策 5） */
  const switchPrerequisites = ref<SwitchPrerequisites | null>(null);
  /** 当前正在切换版本的任务 ID（null = 无切换任务） */
  const switchingTaskId = ref<string | null>(null);
  /** 当前仓库 URL（脱敏后的，F31） */
  const repoUrl = ref<string>("");
  /** 备份列表（F31） */
  const backups = ref<BackupInfo[]>([]);
  /** 是否正在切换仓库地址 / 恢复备份（F31） */
  const switchingRepo = ref(false);

  const loading = ref(false);
  const error = ref<string | null>(null);
  /** requirements 不匹配标记（来自后端 emit "requirements_mismatch"） */
  const requirementsMismatch = ref(false);
  const unlisteners: UnlistenFn[] = [];

  // ========== Getters ==========
  /** 仓库是否已克隆（v3.1 / F26：与后端 is_clone_done 对齐） */
  const isCloned = computed(() => status.value?.is_clone_done ?? false);
  /** 当前版本 tag */
  const currentVersion = computed(() => status.value?.current_version ?? null);
  /** 是否有更新（current_version !== latest_stable 且 latest_stable 非空） */
  const hasUpdates = computed(() => {
    const s = status.value;
    if (!s?.latest_stable) return false;
    return s.current_version !== s.latest_stable;
  });
  /** 工作区是否有未提交改动 */
  const hasLocalChanges = computed(() => status.value?.has_local_changes ?? false);
  /** 是否正在切换版本 */
  const isSwitching = computed(() => switchingTaskId.value !== null);
  /** 是否允许切换版本（前置条件） */
  const canSwitch = computed(() => switchPrerequisites.value?.can_switch ?? false);

  // ========== Actions ==========

  /**
   * 刷新状态（status + tags classified）
   *
   * 同时刷新：
   * - core_status：仓库当前状态
   * - core_list_tags_classified：分类 tag 列表
   * - core_check_switch_prerequisites：前置条件
   */
  async function refresh() {
    loading.value = true;
    error.value = null;
    try {
      const [s, classified] = await Promise.all([
        coreStatus(),
        coreListTagsClassified(),
      ]);
      status.value = s;
      stableTags.value = classified.stable;
      prereleaseTags.value = classified.prerelease;
      // 兼容旧 API：合并为 tags
      tags.value = [...classified.stable, ...classified.prerelease];
      // 前置条件容错刷新（失败不影响主流程）
      try {
        switchPrerequisites.value = await coreCheckSwitchPrerequisites();
      } catch (e) {
        console.warn("[core] refresh prerequisites failed:", e);
      }
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

  /**
   * 切换 ComfyUI 版本（v3.1 / F26 完整 11 步流程）
   *
   * 行为：
   * 1. 后端提交任务，返回 task_id
   * 2. 前端记录 task_id，UI 进入"切换中"状态
   * 3. 监听 task_progress / task_completed 事件
   * 4. 完成后自动 refresh
   *
   * @param targetTag 目标 tag（如 "v0.3.10"）
   * @returns task_id（可用于监听进度 / 取消）
   */
  async function switchVersion(
    targetTag: string,
    mode: "Clean" | "Preserve" | "Skip" = "Preserve",
  ): Promise<string> {
    // 前置二次检查（避免 UI 状态过期）
    const prereq = await coreCheckSwitchPrerequisites();
    switchPrerequisites.value = prereq;
    if (!prereq.can_switch) {
      throw new Error(prereq.block_reason ?? "当前不满足切换版本的前置条件");
    }

    // F36：把 mode 传给后端
    const taskId = await coreSwitchVersionWithMode(targetTag, mode);
    switchingTaskId.value = taskId;
    return taskId;
  }

  /**
   * 切换版本任务完成后的清理
   *
   * 由 task_completed 事件监听器调用，或在 UI 主动取消时调用。
   */
  function clearSwitchingTask() {
    switchingTaskId.value = null;
  }

  /**
   * 确保 models 软链接已建立（v3.1 / F26 决策 12）
   *
   * 在用户修改 models_path 后调用。
   * @returns true 表示链接已建立；false 表示无需链接
   */
  async function ensureModelsLink(): Promise<boolean> {
    return await coreEnsureModelsLink();
  }

  /**
   * 旧版轻量切换（仅 git checkout，不重建 venv）
   *
   * v3.1 / F26 后推荐使用 `switchVersion`。本方法保留用于调试 / 紧急场景。
   */
  async function checkout(tag: string): Promise<CheckoutResult> {
    loading.value = true;
    try {
      const result = await coreCheckout(tag);
      await refresh();
      return result;
    } finally {
      loading.value = false;
    }
  }

  /** 更新到最新稳定版（git pull） */
  async function update() {
    loading.value = true;
    try {
      await coreUpdate();
      await refresh();
    } finally {
      loading.value = false;
    }
  }

  /** 强制刷新 tag 列表（跳过缓存） */
  async function refreshTags(force: boolean = false) {
    try {
      const classified: ClassifiedTags = await coreListTagsClassified(force);
      stableTags.value = classified.stable;
      prereleaseTags.value = classified.prerelease;
      tags.value = [...classified.stable, ...classified.prerelease];
    } catch (e) {
      console.warn("[core] refreshTags failed:", e);
      throw e;
    }
  }

  /** 刷新前置条件（独立调用，用于 UI 实时提示） */
  async function refreshPrerequisites() {
    try {
      switchPrerequisites.value = await coreCheckSwitchPrerequisites();
    } catch (e) {
      console.warn("[core] refreshPrerequisites failed:", e);
    }
  }

  // -----------------------------------------------------------------
  // F31：仓库地址切换与备份恢复
  // -----------------------------------------------------------------

  /** 刷新当前仓库 URL（脱敏后的，F31） */
  async function refreshRepoUrl() {
    try {
      repoUrl.value = await coreGetRepoUrl();
    } catch (e) {
      console.warn("[core] refreshRepoUrl failed:", e);
      throw e;
    }
  }

  /** 刷新备份列表（F31） */
  async function refreshBackups() {
    try {
      backups.value = await coreListBackups();
    } catch (e) {
      console.warn("[core] refreshBackups failed:", e);
      throw e;
    }
  }

  /**
   * 切换仓库地址（F31）
   *
   * 成功后刷新 status + tags + repoUrl + backups（best-effort，不掩盖切换结果）。
   * @returns 后端 SwitchRepoResult（success / rolled_back）
   */
  async function switchRepoUrl(
    url: string,
    migrateCustomNodes: boolean,
  ): Promise<SwitchRepoResult> {
    switchingRepo.value = true;
    try {
      const result = await coreSetRepoUrl(url, migrateCustomNodes);
      try {
        await Promise.all([refresh(), refreshRepoUrl(), refreshBackups()]);
      } catch (e) {
        console.warn("[core] refresh after switchRepoUrl failed:", e);
      }
      return result;
    } finally {
      switchingRepo.value = false;
    }
  }

  /**
   * 恢复备份（F31）
   *
   * 成功后刷新 status + tags + repoUrl + backups（best-effort，不掩盖恢复结果）。
   * @returns 后端 SwitchRepoResult（success / rolled_back）
   */
  async function restoreBackup(backupName: string): Promise<SwitchRepoResult> {
    switchingRepo.value = true;
    try {
      const result = await coreRestoreBackup(backupName);
      try {
        await Promise.all([refresh(), refreshRepoUrl(), refreshBackups()]);
      } catch (e) {
        console.warn("[core] refresh after restoreBackup failed:", e);
      }
      return result;
    } finally {
      switchingRepo.value = false;
    }
  }

  /** 订阅事件 */
  async function subscribe() {
    if (unlisteners.length > 0) return;
    unlisteners.push(
      // 版本切换完成事件（v3.1 / F26）
      await listen<{ from: string | null; to: string }>(
        "core_version_switched",
        (e) => {
          console.info(
            `[core] version switched: ${e.payload.from} → ${e.payload.to}`,
          );
          clearSwitchingTask();
          refresh().catch((err) =>
            console.warn("[core] refresh after switch failed:", err),
          );
        },
      ),
      // requirements 不匹配事件
      await listen<{
        missing: string[];
        outdated: string[];
      }>("requirements_mismatch", (e) => {
        requirementsMismatch.value =
          e.payload.missing.length > 0 || e.payload.outdated.length > 0;
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
    stableTags,
    prereleaseTags,
    switchPrerequisites,
    switchingTaskId,
    repoUrl,
    backups,
    switchingRepo,
    loading,
    error,
    requirementsMismatch,
    // getters
    isCloned,
    currentVersion,
    hasUpdates,
    hasLocalChanges,
    isSwitching,
    canSwitch,
    // actions
    refresh,
    refreshTags,
    refreshPrerequisites,
    refreshRepoUrl,
    refreshBackups,
    switchRepoUrl,
    restoreBackup,
    clone,
    ensureCloned,
    switchVersion,
    clearSwitchingTask,
    ensureModelsLink,
    checkout,
    update,
    subscribe,
    unsubscribe,
  };
});
