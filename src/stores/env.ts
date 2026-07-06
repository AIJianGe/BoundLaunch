/**
 * Environment Store
 *
 * 设计模式：
 * - **Store (Flux)**：集中管理环境信息
 * - **Observer**：监听 `env_changed` / `env_inspect_updated`（F32 新增）事件
 * - **Cache-Aside**：30s TTL 由后端管理，前端仅缓存最近一次结果
 *
 * F32 改造（v3.3）：
 * - `envInfo` 可能为 null（首次启动无 stale 值时）
 * - 6 个长任务 action（createVenv / installTorch / installRequirements /
 *   switchPython / rebuildVenv / changeTorchVariant）改为「invoke 拿 task_id →
 *   waitForTask 等待完成 → refresh」模式
 * - 订阅 `env_inspect_updated` 事件，后台刷新完成后自动更新 envInfo
 * - 删除 `probeTorch`（后端命令已删除）
 * - `checkReadiness` 适配返回 `ReadinessCheckResult | null`
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
  envDiagnose,
  envRepair,
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
  TaskTerminalEvent,
  DiagnoseReport,
  RepairAction,
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
  // v1.8 / F36-Phase2：环境诊断 + 修复
  /** 最新一次诊断报告（null = 未诊断） */
  const lastDiagnose = ref<DiagnoseReport | null>(null);
  /** 修复中状态（true = 已提交 task_id，正在等 task_completed） */
  const repairing = ref(false);
  /** 修复动作（用于进度提示） */
  const currentRepairAction = ref<RepairAction | null>(null);

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

  // ========== F32 内部工具：等待任务完成 ==========

  /**
   * 等待指定 task_id 完成（通过 `task_completed` 事件）
   *
   * F32 改造：长任务命令返回 task_id 后，store 用此方法等待任务终态。
   * - `completed` → resolve
   * - `failed` / `cancelled` → reject(Error)
   *
   * 注意：存在轻微 race condition（任务在 listen 注册前已完成），
   * 但 TaskScheduler 从 submit 到 emit task_completed 至少需要几十毫秒，
   * listen 注册通常在几毫秒内完成，race 概率极低。
   * 若未来需要更稳健，可加 taskGet 轮询兜底。
   */
  function waitForTask(taskId: string): Promise<void> {
    return new Promise((resolve, reject) => {
      let unlisten: UnlistenFn | null = null;
      const cleanup = () => {
        if (unlisten) unlisten();
      };

      // 注册监听器
      listen<TaskTerminalEvent>("task_completed", (e) => {
        if (e.payload.task_id !== taskId) return;
        cleanup();
        if (e.payload.status === "completed") {
          resolve();
        } else {
          reject(
            new Error(
              e.payload.summary ??
                `任务${e.payload.status === "failed" ? "失败" : "已取消"}`,
            ),
          );
        }
      }).then((un) => {
        unlisten = un;
      });
    });
  }

  /**
   * 等待指定 task_id 完成并返回 payload（v3.6 新增）
   *
   * 与 `waitForTask` 类似，但 resolve 时返回后端 `TaskResult.payload`（任意 JSON）。
   * 用于 `env_diagnose` 等需要从任务结果中提取业务数据的命令。
   *
   * @param taskId 任务 ID
   * @returns payload（类型由调用方断言，如 `DiagnoseReport`）
   */
  function waitForTaskWithPayload<T>(taskId: string): Promise<T> {
    return new Promise((resolve, reject) => {
      let unlisten: UnlistenFn | null = null;
      const cleanup = () => {
        if (unlisten) unlisten();
      };

      listen<TaskTerminalEvent>("task_completed", (e) => {
        if (e.payload.task_id !== taskId) return;
        cleanup();
        if (e.payload.status === "completed") {
          resolve(e.payload.payload as T);
        } else {
          reject(
            new Error(
              e.payload.summary ??
                `任务${e.payload.status === "failed" ? "失败" : "已取消"}`,
            ),
          );
        }
      }).then((un) => {
        unlisten = un;
      });
    });
  }

  // ========== Actions ==========

  /**
   * 刷新环境信息（含依赖列表）
   *
   * F32 改造：envInspect 返回 `EnvInfo | null`
   * - 非 null：立即更新 envInfo（可能是 stale 值）
   * - null：首次启动，envInfo 保持 null，等待 `env_inspect_updated` 事件
   */
  async function refresh() {
    loading.value = true;
    error.value = null;
    try {
      const [info, status, deps] = await Promise.all([
        envInspect(),
        envStatus(),
        envListDependencies(),
      ]);
      // F32: info 可能为 null（首次启动），仅在有值时更新
      if (info) {
        envInfo.value = info;
        lastUpdated.value = info.last_updated;
      }
      pythonEnvStatus.value = status;
      // v3.6: deps 可能为 null（首次启动 cache 为空），降级为空数组
      dependencies.value = deps ?? [];
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

  /**
   * 检查环境就绪性（不修改任何后端状态）
   *
   * F32 改造：返回 `ReadinessCheckResult | null`
   * - 非 null：更新 readiness
   * - null：首次启动无 snapshot，readiness 保持 null
   */
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

  /**
   * 创建 venv（F32 改造：返回 task_id，内部等待完成）
   *
   * 流程：invoke 拿 task_id → waitForTask → invalidateCache + refresh
   */
  async function createVenv(pythonVersion: string) {
    loading.value = true;
    try {
      const taskId = await envCreateVenv(pythonVersion);
      await waitForTask(taskId);
      // 完成后失效缓存 + 刷新（env_inspect_updated 事件也会自动更新）
      await envInvalidateCache();
      await refresh();
    } finally {
      loading.value = false;
    }
  }

  /**
   * 安装 torch（F32 改造：返回 task_id，内部等待完成）
   */
  async function installTorch(cudaVersion: string) {
    loading.value = true;
    try {
      const taskId = await envInstallTorch(cudaVersion);
      await waitForTask(taskId);
      await envInvalidateCache();
      await refresh();
    } finally {
      loading.value = false;
    }
  }

  /**
   * 安装 ComfyUI 依赖（F32 改造：返回 task_id，内部等待完成）
   */
  async function installRequirements() {
    loading.value = true;
    try {
      const taskId = await envInstallRequirements();
      await waitForTask(taskId);
      await envInvalidateCache();
      await refresh();
    } finally {
      loading.value = false;
    }
  }

  /**
   * 切换 Python 版本（F32 改造：返回 task_id，内部等待完成）
   */
  async function switchPython(pythonVersion: string) {
    loading.value = true;
    try {
      const taskId = await envSwitchPython(pythonVersion);
      await waitForTask(taskId);
      await envInvalidateCache();
      await refresh();
    } finally {
      loading.value = false;
    }
  }

  /** 检查兼容性 */
  async function checkCompatibility(): Promise<CompatibilityResult> {
    return envCheckCompatibility();
  }

  /**
   * 重建 venv（F32 改造：返回 task_id，内部等待完成）
   */
  async function rebuildVenv() {
    loading.value = true;
    try {
      const taskId = await envRebuildVenv();
      await waitForTask(taskId);
      await envInvalidateCache();
      await refresh();
    } finally {
      loading.value = false;
    }
  }

  // ========== v1.8 / F36-Phase2：环境诊断 + 修复 ==========

  /**
   * 环境诊断（v1.8 / F36-Phase2）
   *
   * v3.6 改造：从同步命令改为 TaskScheduler 任务。
   * - 调 `envDiagnose()` 拿 task_id → `waitForTaskWithPayload<DiagnoseReport>` 等终态
   * - 诊断完成后后端已 emit `RequirementsInstalled` → env cache 失效 →
   *   `env_inspect_updated` 事件，store 的 subscribe 会自动更新 envInfo。
   * - 为确保 UI 立即刷新（不等事件），这里显式调 `envInvalidateCache() + refresh()`。
   *
   * 返回 `DiagnoseReport`：
   * - `venv_exists` / `torch_import_ok` / `torch_version`
   * - `issues[]`：诊断出的所有问题（按严重度排序）
   * - `suggested_action`：综合建议（最严重 action）
   * - `suggested_reason`：建议原因（用户可读）
   */
  async function diagnose(): Promise<DiagnoseReport> {
    const taskId = await envDiagnose();
    const report = await waitForTaskWithPayload<DiagnoseReport>(taskId);
    lastDiagnose.value = report;
    // v3.6：强制 invalidate + refresh，确保 UI 立即反映最新状态
    // （后端虽已 emit RequirementsInstalled 触发后台刷新，
    //   但显式 refresh 能让前端立即拿到最新数据，不依赖事件时序）
    try {
      await envInvalidateCache();
      await refresh();
    } catch (e) {
      console.warn("[env] post-diagnose refresh failed:", e);
    }
    return report;
  }

  /**
   * 环境修复（v1.8 / F36-Phase2）
   *
   * F32 改造：返回 task_id，内部等待完成。
   * 完成后 invalidateCache + refresh（让 StatusCard / 关键依赖列表更新）。
   *
   * @param action 修复动作（建议从 `diagnose` 拿到的 `suggested_action` 传入）
   */
  async function repair(action: RepairAction) {
    if (repairing.value) {
      throw new Error("已有修复任务在执行中");
    }
    repairing.value = true;
    currentRepairAction.value = action;
    try {
      const taskId = await envRepair(action);
      await waitForTask(taskId);
      // 完成后让缓存失效 + 重新刷新 + 重新诊断
      await envInvalidateCache();
      await refresh();
      // 重新诊断（让 UI 看到修复结果）
      try {
        await diagnose();
      } catch (e) {
        console.warn("[env] re-diagnose after repair failed:", e);
      }
    } finally {
      repairing.value = false;
      currentRepairAction.value = null;
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
   * 切换 torch 变体（F32 改造：返回 task_id，内部等待完成）
   *
   * 流程（action 内部）：停 ComfyUI → 切换 torch → 更新 Config
   */
  async function changeTorchVariant(variant: TorchVariant) {
    switchingTorch.value = true;
    try {
      const taskId = await envChangeTorchVariant(variant);
      await waitForTask(taskId);
      // 同步本地状态
      currentTorch.value = variant;
      // 同步 ConfigStore（后端 action 已更新 Config，这里同步前端缓存）
      const configStore = useConfigStore();
      const raw = serializeTorchVariant(variant);
      await configStore.update({
        torch: {
          cuda_version: "cpu", // 老字段，新逻辑由 torch_variant 决定；安全 fallback
          torch_variant: raw,
        } as any,
      });
      // 失效后端 30s 缓存 + 刷新
      await envInvalidateCache();
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
   * 订阅后端事件
   *
   * F32 改造：新增 `env_inspect_updated` 事件订阅
   * - `env_changed`：环境状态变更（旧事件，保留兼容）
   * - `env_inspect_updated`（F32 新增）：后台 spawn_refresh 完成，payload = 新 EnvInfo
   *   收到后自动更新 envInfo（无需主动 refresh）
   */
  async function subscribe() {
    if (unlisteners.length > 0) return;
    unlisteners.push(
      // 旧事件：环境变更 → 主动 refresh
      await listen<void>("env_changed", () => {
        refresh().catch((e) => console.warn("env refresh failed:", e));
      }),
      // F32 新事件：后台刷新完成 → 直接更新 envInfo（不调 refresh，避免循环）
      await listen<EnvInfo>("env_inspect_updated", (e) => {
        envInfo.value = e.payload;
        lastUpdated.value = e.payload.last_updated;
        // 顺便重新检查 readiness（基于新 snapshot）
        checkReadiness().catch((err) =>
          console.warn("[env] readiness check after env_inspect_updated failed:", err),
        );
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
    // v1.8 / F36-Phase2 state
    lastDiagnose,
    repairing,
    currentRepairAction,
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
    createVenv,
    installTorch,
    installRequirements,
    switchPython,
    checkCompatibility,
    rebuildVenv,
    checkConflicts,
    // v1.8 / F36-Phase2 actions
    diagnose,
    repair,
    // v3.0 actions (F25)
    detectGpus,
    recommendTorch,
    changeTorchVariant,
    refreshGpuAndRecommendation,
    subscribe,
    unsubscribe,
  };
});
