/**
 * Environment Store
 *
 * 设计模式：
 * - **Store (Flux)**：集中管理环境信息
 * - **Observer**：监听 `env_changed` 事件
 * - **Cache-Aside**：30s TTL 由后端管理，前端仅缓存最近一次结果
 *
 * 使用方式：
 * ```ts
 * const envStore = useEnvStore();
 * await envStore.subscribe();
 * await envStore.refresh();
 * ```
 */

import { defineStore } from "pinia";
import { ref, computed } from "vue";
import {
  envInspect,
  envProbeTorch,
  envListDependencies,
  envInvalidateCache,
  envReadinessCheck,
  envStatus,
  envCreateVenv,
  envInstallTorch,
  envSwitchPython,
  envCheckCompatibility,
  envRebuildVenv,
} from "@/api/env";
import { listen, type UnlistenFn } from "@/api";
import type {
  EnvInfo,
  DependencyInfo,
  PythonEnvStatus,
  CompatibilityResult,
  ReadinessCheckResult,
} from "@/api/types";

export const useEnvStore = defineStore("env", () => {
  // ========== State ==========
  const envInfo = ref<EnvInfo | null>(null);
  const pythonEnvStatus = ref<PythonEnvStatus | null>(null);
  const dependencies = ref<DependencyInfo[]>([]);
  const readiness = ref<ReadinessCheckResult | null>(null);
  const loading = ref(false);
  const checkingReadiness = ref(false);
  const error = ref<string | null>(null);
  const lastUpdated = ref<string | null>(null);

  const unlisteners: UnlistenFn[] = [];

  // ========== Getters ==========
  const isLoaded = computed(() => envInfo.value !== null);
  const torchInstalled = computed(() => envInfo.value?.torch_installed ?? false);
  const cudaAvailable = computed(() => envInfo.value?.cuda_available ?? false);
  const comfyuiCloned = computed(() => envInfo.value?.comfyui_cloned ?? false);
  const venvExists = computed(() => pythonEnvStatus.value?.venv_exists ?? false);
  const uvAvailable = computed(() => pythonEnvStatus.value?.uv_installed ?? false);
  /** 是否就绪（readiness.ready === true），false 时按钮变 "一键安装" */
  const isReady = computed(() => readiness.value?.ready ?? false);

  // ========== Actions ==========

  /** 刷新环境信息（含依赖列表） */
  async function refresh() {
    loading.value = true;
    error.value = null;
    try {
      const [info, status, deps] = await Promise.all([
        envInspect(),
        envStatus(),
        envListDependencies(),
      ]);
      envInfo.value = info;
      pythonEnvStatus.value = status;
      dependencies.value = deps;
      lastUpdated.value = info.last_updated;
      // 顺便做一次 readiness 检查（不抛错）
      checkReadiness().catch((e) =>
        console.warn("[env] readiness check failed:", e),
      );
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
      throw e;
    } finally {
      loading.value = false;
    }
  }

  /** 检查环境就绪性（不修改任何后端状态） */
  async function checkReadiness() {
    checkingReadiness.value = true;
    try {
      readiness.value = await envReadinessCheck();
    } finally {
      checkingReadiness.value = false;
    }
  }

  /** 强制清除缓存（下次 refresh 重新检测） */
  async function invalidateCache() {
    await envInvalidateCache();
    await refresh();
  }

  /** 探测 torch（真实调用 torch.cuda.is_available） */
  async function probeTorch() {
    const result = await envProbeTorch();
    // 触发刷新以更新 envInfo
    await refresh();
    return result;
  }

  /** 创建 venv */
  async function createVenv(pythonVersion: string) {
    loading.value = true;
    try {
      await envCreateVenv(pythonVersion);
      await refresh();
    } finally {
      loading.value = false;
    }
  }

  /** 安装 torch */
  async function installTorch(cudaVersion: string) {
    loading.value = true;
    try {
      await envInstallTorch(cudaVersion);
      await refresh();
    } finally {
      loading.value = false;
    }
  }

  /** 切换 Python 版本（5 步事务，自动回滚） */
  async function switchPython(pythonVersion: string) {
    loading.value = true;
    try {
      await envSwitchPython(pythonVersion);
      await refresh();
    } finally {
      loading.value = false;
    }
  }

  /** 检查兼容性 */
  async function checkCompatibility(): Promise<CompatibilityResult> {
    return envCheckCompatibility();
  }

  /** 重建 venv */
  async function rebuildVenv() {
    loading.value = true;
    try {
      await envRebuildVenv();
      await refresh();
    } finally {
      loading.value = false;
    }
  }

  /**
   * 订阅后端 `env_changed` 事件
   *
   * 后端在 venv 创建 / torch 安装 / Python 切换等操作后 emit 此事件。
   */
  async function subscribe() {
    if (unlisteners.length > 0) return;
    unlisteners.push(
      await listen<void>("env_changed", () => {
        // 收到事件后自动刷新
        refresh().catch((e) => console.warn("env refresh failed:", e));
      }),
    );
  }

  function unsubscribe() {
    unlisteners.forEach((un) => un());
    unlisteners.length = 0;
  }

  return {
    // state
    envInfo,
    pythonEnvStatus,
    dependencies,
    readiness,
    loading,
    checkingReadiness,
    error,
    lastUpdated,
    // getters
    isLoaded,
    torchInstalled,
    cudaAvailable,
    comfyuiCloned,
    venvExists,
    uvAvailable,
    isReady,
    // actions
    refresh,
    checkReadiness,
    invalidateCache,
    probeTorch,
    createVenv,
    installTorch,
    switchPython,
    checkCompatibility,
    rebuildVenv,
    subscribe,
    unsubscribe,
  };
});
