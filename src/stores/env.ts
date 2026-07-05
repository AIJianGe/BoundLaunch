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
  envInstallRequirements,
  envSwitchPython,
  envCheckCompatibility,
  envRebuildVenv,
  envCheckDependencyConflicts,
  envChangeTorchVariant,
  systemDetectGpus,
  systemClearGpuCache,
  systemRecommendTorch,
} from "@/api/env";
import { listen, type UnlistenFn } from "@/api";
import type {
  EnvInfo,
  DependencyInfo,
  PythonEnvStatus,
  CompatibilityResult,
  ReadinessCheckResult,
  ConflictReport,
  TorchVariant,
  GpuInfo,
} from "@/api/types";
import { parseTorchVariant, serializeTorchVariant } from "@/utils/torchVariant";
import { useConfigStore } from "./config";

export const useEnvStore = defineStore("env", () => {
  // ========== State ==========
  const envInfo = ref<EnvInfo | null>(null);
  const pythonEnvStatus = ref<PythonEnvStatus | null>(null);
  const dependencies = ref<DependencyInfo[]>([]);
  // v3.0 依赖冲突检测结果
  const conflictReport = ref<ConflictReport | null>(null);
  const readiness = ref<ReadinessCheckResult | null>(null);
  // v3.0 torch 多厂商 + GPU 检测（F25）
  const gpus = ref<GpuInfo[]>([]);
  const recommendedTorch = ref<TorchVariant | null>(null);
  const currentTorch = ref<TorchVariant | null>(null); // 当前 venv 中安装的 torch（解析自 Config.torch.torch_variant）
  const switchingTorch = ref(false);
  const detectingGpus = ref(false);
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

  /** v3.0：当前选中的 torch 变体（来自 ConfigStore.torch.torch_variant） */
  const activeTorch = computed<TorchVariant | null>(() => {
    // 优先用 store 自己解析的 currentTorch（refresh 时写入）
    if (currentTorch.value) return currentTorch.value;
    // fallback: 从 config store 拿原始 JSON 字符串解析
    const configStore = useConfigStore();
    const raw = configStore.config?.torch.torch_variant;
    return raw ? parseTorchVariant(raw) : null;
  });

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
      // 同步 v3.0 torch 变体（从 Config 解析）
      const configStore = useConfigStore();
      const raw = configStore.config?.torch.torch_variant;
      currentTorch.value = raw ? parseTorchVariant(raw) : null;
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

  /** 安装 ComfyUI 依赖（v2.14，幂等：uv 自动跳过已满足的包） */
  async function installRequirements() {
    loading.value = true;
    try {
      await envInstallRequirements();
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

  /** v3.0 依赖冲突检测（扫描 custom_nodes 下的所有 requirements 文件） */
  async function checkConflicts(): Promise<ConflictReport | null> {
    try {
      conflictReport.value = await envCheckDependencyConflicts();
      return conflictReport.value;
    } catch (e) {
      console.warn("[env] checkConflicts failed:", e);
      conflictReport.value = null;
      return null;
    }
  }

  // ========== v3.0 torch 多厂商 + GPU 检测 actions（F25） ==========

  /**
   * 检测所有 GPU（带 5 分钟缓存）
   *
   * @param forceRefresh 强制刷新（清除缓存重新检测），默认 false
   */
  async function detectGpus(forceRefresh = false): Promise<GpuInfo[]> {
    detectingGpus.value = true;
    try {
      gpus.value = await systemDetectGpus(forceRefresh);
      return gpus.value;
    } finally {
      detectingGpus.value = false;
    }
  }

  /** 智能推荐 torch 变体（基于 GPU 检测 + OS 平台） */
  async function recommendTorch(): Promise<TorchVariant | null> {
    try {
      recommendedTorch.value = await systemRecommendTorch();
      return recommendedTorch.value;
    } catch (e) {
      console.warn("[env] recommendTorch failed:", e);
      recommendedTorch.value = null;
      return null;
    }
  }

  /**
   * 切换 torch 变体
   *
   * 流程：
   * 1. 自动停 ComfyUI（如运行中，调用方需在 UI 提示用户）
   * 2. uv pip install --upgrade <torch> + 验证
   * 3. 更新 Config（cuda_version + torch_variant）
   * 4. 失效 env_status 缓存
   *
   * 失败时返回错误，旧 torch 保留。
   */
  async function changeTorchVariant(variant: TorchVariant) {
    switchingTorch.value = true;
    try {
      await envChangeTorchVariant(variant);
      // 同步本地状态
      currentTorch.value = variant;
      // 同步 ConfigStore
      const configStore = useConfigStore();
      const raw = serializeTorchVariant(variant);
      await configStore.update({
        torch: {
          cuda_version: "cpu", // 老字段，新逻辑由 torch_variant 决定；安全 fallback
          torch_variant: raw,
        } as any,
      });
      // 刷新环境信息
      await refresh();
    } finally {
      switchingTorch.value = false;
    }
  }

  /** 清除 GPU 缓存 + 重新检测 + 重新推荐 */
  async function refreshGpuAndRecommendation() {
    await systemClearGpuCache();
    await detectGpus(true);
    await recommendTorch();
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
    conflictReport,
    // v3.0 state (F25)
    gpus,
    recommendedTorch,
    currentTorch,
    switchingTorch,
    detectingGpus,
    // getters
    isLoaded,
    torchInstalled,
    cudaAvailable,
    comfyuiCloned,
    venvExists,
    uvAvailable,
    isReady,
    activeTorch,
    // actions
    refresh,
    checkReadiness,
    invalidateCache,
    probeTorch,
    createVenv,
    installTorch,
    installRequirements,
    switchPython,
    checkCompatibility,
    rebuildVenv,
    checkConflicts,
    // v3.0 actions (F25)
    detectGpus,
    recommendTorch,
    changeTorchVariant,
    refreshGpuAndRecommendation,
    subscribe,
    unsubscribe,
  };
});
