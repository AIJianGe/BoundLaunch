/**
 * useSwitchVersion — 版本切换专用 composable（v3.5 全新）
 *
 * 设计模式：**Template Method** + **Observer**
 * - 封装"提交 → 跟踪进度 → 接收结果 / 错误 / 取消"的完整流程
 * - 复用 useTaskProgress（progress / log / cancel）
 * - UI 只关心 onComplete / onError 回调，不用自己写事件监听
 *
 * 使用场景：
 * - CoreVersionPage.vue：用户确认切换后调用
 * - SwitchVersionDialog.vue：用户点击"确认切换"按钮
 *
 * 使用方式：
 * ```ts
 * const { progress, message, logs, isRunning, isCompleted, errorSummary,
 *         cancel, start } = useSwitchVersion();
 *
 * async function onConfirm(mode: SwitchMode) {
 *   await start(targetTag, mode, {
 *     onComplete: (summary) => toast.success('切换成功: ' + summary),
 *     onError: (msg) => toast.error('切换失败', msg ?? ''),
 *   });
 * }
 * ```
 *
 * v3.5 关键改进：
 * - 不再有 timeout：取消由用户主动触发（cancel()）
 * - 实时日志：logs 数组实时累积子进程输出
 * - 进度聚合：父任务 + 子任务进度统一显示
 * - 失败回滚：后端自动处理，前端只接收 onError
 */

import { ref, computed } from "vue";
import { useTaskProgress, type TaskLogLine } from "./useTaskProgress";
import {
  coreSwitchVersionWithMode,
  type SwitchMode,
  type VersionCompatReport,
} from "@/api/core";
import {
  coreCheckVersionCompatibility,
  coreCheckSwitchPrerequisites,
} from "@/api/core";
import { taskGet } from "@/api/task";
import { listen, type UnlistenFn } from "@/api";

/** useSwitchVersion 回调 */
export interface SwitchCallbacks {
  /** 切换成功 */
  onComplete?: (summary: string | null) => void;
  /** 切换失败（含取消） */
  onError?: (summary: string | null) => void;
  /** 进度更新 */
  onProgress?: (progress: number, message: string | null) => void;
  /** 实时日志 */
  onLog?: (log: TaskLogLine) => void;
}

/**
 * 版本切换 composable
 *
 * 内部使用 useTaskProgress 跟踪 task_progress / task_completed / task_log 事件。
 * 暴露 progress / message / logs / isRunning / isCompleted / errorSummary 响应式状态。
 */
export function useSwitchVersion() {
  // 复用 useTaskProgress 提供核心跟踪能力（必须在 setup 同步阶段调用）
  const tracker = useTaskProgress();

  /** 切换模式（用于展示当前正在执行哪种模式） */
  const currentMode = ref<SwitchMode | null>(null);
  /** 目标 tag */
  const currentTargetTag = ref<string | null>(null);

  // ========== 派生 ==========
  const isRunning = computed(() => tracker.isRunning.value);
  const isCompleted = computed(() => tracker.isCompleted.value);
  const isFailed = computed(() => tracker.isFailed.value);
  const isCancelled = computed(() => tracker.status.value === "cancelled");
  const progress = computed(() => tracker.progress.value);
  const message = computed(() => tracker.message.value);
  const errorSummary = computed(() => tracker.errorSummary.value);
  const logs = computed(() => tracker.logs.value);

  // ========== Actions ==========

  /**
   * 开始切换版本
   *
   * 流程：
   * 1. 调 coreSwitchVersionWithMode 提交 task（v3.5 异步，立即返回 task_id）
   * 2. useTaskProgress 跟踪 progress / log / completed
   * 3. 完成后触发 cb.onComplete / cb.onError
   *
   * @param targetTag 目标 tag（如 "v0.3.10"）
   * @param mode 切换模式
   * @param cb 回调
   */
  async function start(
    targetTag: string,
    mode: SwitchMode,
    cb?: SwitchCallbacks,
  ): Promise<string> {
    currentMode.value = mode;
    currentTargetTag.value = targetTag;

    // 1. 提交 task（返回 task_id，不阻塞）
    let taskId: string;
    try {
      taskId = await coreSwitchVersionWithMode(targetTag, mode);
    } catch (e) {
      // 提交失败（如队列满、ComfyUI 进程已运行）→ 直接 onError
      const msg = e instanceof Error ? e.message : String(e);
      cb?.onError?.(msg);
      throw e;
    }

    // 2. 启动跟踪
    await tracker.trackTask(taskId, {
      onProgress: (p, m) => cb?.onProgress?.(p, m),
      onLog: (log) => cb?.onLog?.(log),
      onComplete: (summary) => {
        currentMode.value = null;
        currentTargetTag.value = null;
        cb?.onComplete?.(summary);
      },
      onError: (summary) => {
        currentMode.value = null;
        currentTargetTag.value = null;
        cb?.onError?.(summary);
      },
    });

    return taskId;
  }

  /**
   * 取消当前切换
   *
   * 调后端 task_cancel（幂等），级联取消所有子任务并触发 git 回滚。
   */
  async function cancel() {
    await tracker.cancel();
  }

  /** 重置 composable 状态（用于复用） */
  function reset() {
    currentMode.value = null;
    currentTargetTag.value = null;
    tracker.reset();
  }

  return {
    // state
    currentMode,
    currentTargetTag,
    // derived
    isRunning,
    isCompleted,
    isFailed,
    isCancelled,
    progress,
    message,
    errorSummary,
    logs,
    // actions
    start,
    cancel,
    reset,
    // 透传 taskId 访问
    currentTaskId: tracker.currentTaskId,
  };
}

// ====================================================================
// useCheckCompat — 版本兼容性预检 composable（v3.5 异步化）
// ====================================================================

/** useCheckCompat 返回值 */
export interface CheckCompatReturn {
  /** 是否正在加载 */
  loading: import("vue").Ref<boolean>;
  /** 兼容性报告（完成后填充） */
  report: import("vue").Ref<VersionCompatReport | null>;
  /** 错误信息 */
  error: import("vue").Ref<string | null>;
  /** 进度（0-100） */
  progress: import("vue").Ref<number>;
  /** 消息 */
  message: import("vue").Ref<string | null>;
  /** 实时日志 */
  logs: import("vue").Ref<TaskLogLine[]>;
  /** 启动检查 */
  check: (targetTag: string) => Promise<void>;
  /** 取消 */
  cancel: () => Promise<void>;
  /** 重置 */
  reset: () => void;
}

/**
 * 版本兼容性预检 composable（v3.5 异步化）
 *
 * 切换版本前的兼容性检查（git show target:requirements.txt + 解析差异 + 推荐模式）。
 * 后端提交为 CheckCompat 任务（Low 优先级），不阻塞 UI。
 *
 * 关键设计：
 * - 在 composable 顶层调 useTaskProgress（setup 同步阶段），
 *   通过内部 taskId 状态在 check() 中切换跟踪目标。
 * - 实时日志通过 task_log 事件累积，cancel 支持中途取消。
 *
 * 用法：
 * ```ts
 * const compat = useCheckCompat();
 * await compat.check(targetTag);
 * if (compat.report.value) {
 *   // 显示推荐模式
 * }
 * ```
 */
export function useCheckCompat(): CheckCompatReturn {
  const loading = ref(false);
  const report = ref<VersionCompatReport | null>(null);
  const error = ref<string | null>(null);
  const progress = ref(0);
  const message = ref<string | null>(null);
  const logs = ref<TaskLogLine[]>([]);

  // 在 setup 顶层调 useTaskProgress（保证 onUnmounted 注册正确）
  const tracker = useTaskProgress();
  let compatUnlisten: UnlistenFn | null = null;
  let currentCompatTaskId: string | null = null;

  async function check(targetTag: string) {
    // 清理上一次结果
    report.value = null;
    error.value = null;
    progress.value = 0;
    message.value = null;
    logs.value = [];
    loading.value = true;

    // 清理前一个 listener
    if (compatUnlisten) {
      compatUnlisten();
      compatUnlisten = null;
    }

    // 提交 task
    let taskId: string;
    try {
      taskId = await coreCheckVersionCompatibility(targetTag);
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
      loading.value = false;
      return;
    }
    currentCompatTaskId = taskId;

    // 监听 task_completed 拿 payload
    const { listen: tauriListen } = await import("@/api");
    compatUnlisten = await tauriListen<{
      task_id: string;
      status: string;
      summary: string | null;
      payload?: VersionCompatReport;
    }>("task_completed", async (e) => {
      if (e.payload.task_id !== taskId) return;
      if (e.payload.status === "completed") {
        // v3.5：从 task_completed.payload 读 VersionCompatReport
        const payload = e.payload.payload;
        if (payload) {
          report.value = payload;
        } else {
          // 兜底：通过 task_get 查询
          try {
            const info = await taskGet(taskId);
            if (info?.status?.phase === "completed") {
              const trPayload = (
                info.status as unknown as { payload?: VersionCompatReport }
              ).payload;
              if (trPayload) report.value = trPayload;
            }
          } catch (e) {
            console.warn("[useCheckCompat] taskGet fallback failed:", e);
          }
        }
        loading.value = false;
      } else {
        error.value = e.payload.summary ?? "兼容性检查失败";
        loading.value = false;
      }
      // 清理 listener
      if (compatUnlisten) {
        compatUnlisten();
        compatUnlisten = null;
      }
    });

    // 通过 useTaskProgress 跟踪 progress / log
    await tracker.trackTask(taskId, {
      onProgress: (p, m) => {
        progress.value = p;
        message.value = m;
      },
      onLog: (log) => {
        logs.value.push(log);
        if (logs.value.length > 500) {
          logs.value.splice(0, logs.value.length - 500);
        }
      },
    });
  }

  async function cancel() {
    if (currentCompatTaskId) {
      // 走 useTaskProgress 的 cancel 路径（调 task_cancel）
      await tracker.cancel();
    }
  }

  function reset() {
    report.value = null;
    error.value = null;
    progress.value = 0;
    message.value = null;
    logs.value = [];
    loading.value = false;
    if (compatUnlisten) {
      compatUnlisten();
      compatUnlisten = null;
    }
    tracker.reset();
  }

  return { loading, report, error, progress, message, logs, check, cancel, reset };
}

// ====================================================================
// useCheckPrereq — 切换前置条件检查 composable
// ====================================================================

/** useCheckPrereq 返回值 */
export interface CheckPrereqReturn {
  /** 是否正在加载 */
  loading: import("vue").Ref<boolean>;
  /** 前置条件结果 */
  result: import("vue").Ref<{
    can_switch: boolean;
    comfyui_running: boolean;
    has_local_changes: boolean;
    current_tag: string | null;
    block_reason: string | null;
  } | null>;
  /** 错误信息 */
  error: import("vue").Ref<string | null>;
  /** 进度（0-100） */
  progress: import("vue").Ref<number>;
  /** 消息 */
  message: import("vue").Ref<string | null>;
  /** 启动检查 */
  check: () => Promise<void>;
  /** 取消 */
  cancel: () => Promise<void>;
  /** 重置 */
  reset: () => void;
}

import type { SwitchPrerequisites } from "@/api/types";

/**
 * 切换前置条件检查 composable（v3.5 异步化）
 *
 * 后端提交为 CheckPrereq 任务（High 优先级），不阻塞 UI。
 */
export function useCheckPrereq(): CheckPrereqReturn {
  const loading = ref(false);
  const result = ref<SwitchPrerequisites | null>(null);
  const error = ref<string | null>(null);
  const progress = ref(0);
  const message = ref<string | null>(null);

  const tracker = useTaskProgress();
  let prereqUnlisten: UnlistenFn | null = null;
  let currentTaskId: string | null = null;

  async function check() {
    result.value = null;
    error.value = null;
    progress.value = 0;
    message.value = null;
    loading.value = true;

    if (prereqUnlisten) {
      prereqUnlisten();
      prereqUnlisten = null;
    }

    let taskId: string;
    try {
      taskId = await coreCheckSwitchPrerequisites();
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
      loading.value = false;
      return;
    }
    currentTaskId = taskId;

    // 监听 task_completed
    prereqUnlisten = await listen<{
      task_id: string;
      status: string;
      summary: string | null;
      payload?: SwitchPrerequisites;
    }>("task_completed", async (e) => {
      if (e.payload.task_id !== taskId) return;
      if (e.payload.status === "completed") {
        const payload = e.payload.payload;
        if (payload) {
          result.value = payload;
        } else {
          // 兜底
          try {
            const info = await taskGet(taskId);
            if (info?.status?.phase === "completed") {
              const trPayload = (
                info.status as unknown as { payload?: SwitchPrerequisites }
              ).payload;
              if (trPayload) result.value = trPayload;
            }
          } catch (e) {
            console.warn("[useCheckPrereq] taskGet fallback failed:", e);
          }
        }
        loading.value = false;
      } else {
        error.value = e.payload.summary ?? "前置条件检查失败";
        loading.value = false;
      }
      if (prereqUnlisten) {
        prereqUnlisten();
        prereqUnlisten = null;
      }
    });

    await tracker.trackTask(taskId, {
      onProgress: (p, m) => {
        progress.value = p;
        message.value = m;
      },
    });
  }

  async function cancel() {
    if (currentTaskId) {
      await tracker.cancel();
    }
  }

  function reset() {
    result.value = null;
    error.value = null;
    progress.value = 0;
    message.value = null;
    loading.value = false;
    if (prereqUnlisten) {
      prereqUnlisten();
      prereqUnlisten = null;
    }
    tracker.reset();
  }

  return { loading, result, error, progress, message, check, cancel, reset };
}
