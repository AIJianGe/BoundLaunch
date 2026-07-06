/**
 * useTaskProgress — F32 任务进度跟踪 composable
 *
 * 设计模式：**Observer** - 订阅 TaskScheduler 的 `task_progress` / `task_completed` 事件
 *
 * 使用场景：
 * - OnboardingPage：创建 venv / 安装 torch / 安装依赖时显示进度条
 * - SettingsPage：切换 torch 变体 / 切换 Python 版本 / 重建 venv 时显示进度
 * - 首页「一键补装」按钮：显示 InstallRequirements 进度
 *
 * 使用方式：
 * ```ts
 * const { progress, message, isRunning, trackTask } = useTaskProgress();
 *
 * async function createVenv() {
 *   const taskId = await envCreateVenv('3.11');
 *   await trackTask(taskId, {
 *     onComplete: () => toast.success('venv 创建成功'),
 *     onError: (msg) => toast.error('venv 创建失败', msg ?? ''),
 *   });
 * }
 * ```
 *
 * 详见 `PR/06-界面设计.md §5.7 任务进度组件`
 */

import { ref, onUnmounted } from "vue";
import { listen, type UnlistenFn } from "@/api";
import type { TaskProgressEvent, TaskTerminalEvent } from "@/api/types";

/** 任务回调（终态触发一次，进度持续触发） */
export interface TaskCallbacks {
  /** 任务成功完成时触发（summary 为后端 TaskResult.summary） */
  onComplete?: (summary: string | null) => void;
  /** 任务失败或取消时触发（summary 为错误信息） */
  onError?: (summary: string | null) => void;
  /** 进度更新时触发（0..=100） */
  onProgress?: (progress: number, message: string | null) => void;
}

/**
 * 任务进度跟踪 composable
 *
 * 每次调用创建一个独立的跟踪器，内部订阅全局 `task_progress` / `task_completed` 事件，
 * 通过 `currentTaskId` 过滤只处理自己关心的任务。
 *
 * 组件卸载时自动清理监听器（onUnmounted）。
 */
export function useTaskProgress() {
  // ========== State ==========
  /** 当前跟踪的 task_id（null 表示未跟踪） */
  const currentTaskId = ref<string | null>(null);
  /** 进度百分比 0..=100 */
  const progress = ref(0);
  /** 人类可读消息（如 "下载 torch wheel..."） */
  const message = ref<string | null>(null);
  /** 当前状态字符串：queued / running / completed / failed / cancelled */
  const status = ref<string>("queued");
  /** 是否运行中（queued 或 running） */
  const isRunning = ref(false);
  /** 是否成功完成 */
  const isCompleted = ref(false);
  /** 是否失败或取消 */
  const isFailed = ref(false);

  // ========== Internal ==========
  const unlisteners: UnlistenFn[] = [];
  let callbacks: TaskCallbacks = {};

  // ========== Actions ==========

  /**
   * 设置事件监听器（订阅 task_progress / task_completed）
   *
   * 内部方法，由 trackTask 调用。
   */
  async function setupListeners() {
    unlisteners.push(
      await listen<TaskProgressEvent>("task_progress", (e) => {
        if (e.payload.task_id !== currentTaskId.value) return;
        progress.value = e.payload.progress;
        message.value = e.payload.message;
        status.value = e.payload.status;
        callbacks.onProgress?.(e.payload.progress, e.payload.message);
      }),
      await listen<TaskTerminalEvent>("task_completed", (e) => {
        if (e.payload.task_id !== currentTaskId.value) return;
        status.value = e.payload.status;
        isRunning.value = false;
        if (e.payload.status === "completed") {
          isCompleted.value = true;
          progress.value = 100;
          callbacks.onComplete?.(e.payload.summary);
        } else if (e.payload.status === "failed" || e.payload.status === "cancelled") {
          isFailed.value = true;
          callbacks.onError?.(e.payload.summary);
        }
        // 终态后清理监听器（避免重复触发）
        cleanup();
      }),
    );
  }

  /** 清理所有事件监听器 */
  function cleanup() {
    unlisteners.forEach((un) => un());
    unlisteners.length = 0;
  }

  /**
   * 跟踪指定任务
   *
   * 调用此方法后，composable 会自动订阅事件并更新响应式状态。
   * 任务进入终态（completed / failed / cancelled）后会自动清理监听器。
   *
   * @param taskId 任务 ID（来自 invoke 返回值）
   * @param cb 回调（可选）
   */
  async function trackTask(taskId: string, cb?: TaskCallbacks) {
    // 若已有跟踪中的任务，先清理
    cleanup();

    currentTaskId.value = taskId;
    callbacks = cb ?? {};
    isRunning.value = true;
    isCompleted.value = false;
    isFailed.value = false;
    progress.value = 0;
    message.value = null;
    status.value = "queued";

    await setupListeners();
  }

  /** 重置状态（不清理监听器，用于复用 composable） */
  function reset() {
    currentTaskId.value = null;
    progress.value = 0;
    message.value = null;
    status.value = "queued";
    isRunning.value = false;
    isCompleted.value = false;
    isFailed.value = false;
    callbacks = {};
  }

  // 组件卸载时自动清理
  onUnmounted(() => {
    cleanup();
  });

  return {
    // state
    currentTaskId,
    progress,
    message,
    status,
    isRunning,
    isCompleted,
    isFailed,
    // actions
    trackTask,
    cleanup,
    reset,
  };
}
