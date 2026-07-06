/**
 * useStartComfyui — v3.4 启动 ComfyUI 的专用 composable
 *
 * 设计模式：**Facade** - 包装 processStart / useTaskProgress / 跳转逻辑
 *
 * 与 `useTaskProgress` 的关系：
 * - `useTaskProgress` 是通用的"任务进度跟踪器"（订阅 task_progress / task_completed）
 * - `useStartComfyui` 在它之上加了"启动 ComfyUI 特定的业务逻辑"：
 *   - 自动跳转到 /logs
 *   - 失败时弹窗展示 stderr tail
 *   - 跟踪 process_crashed 事件（5s~60s 期间 child 死亡）
 *
 * ## 启动流程
 *
 * ```
 * [点启动] → start()
 *   ├─ 调 processStart() → 后端立即返回 task_id（不阻塞）
 *   ├─ trackTask(taskId) → 订阅 task_progress / task_completed
 *   ├─ router.push('/logs') → 用户立即看到实时进度 + 日志
 *   └─ 终态时:
 *      ├─ 成功 → toast.success + 等待 process_started 事件（health_check 通过）
 *      ├─ 失败 → toast.error + 弹窗显示 stderr tail
 *      └─ 5s~60s 期间 child 死 → process_crashed 事件 → 弹窗 + 跳 /logs
 * ```
 *
 * ## 行为
 *
 * - **始终跳转 /logs**：让用户看到实时进度 + 详细日志（F25 哲学：透明）
 * - **失败弹窗不阻塞**：stderr tail 在 NModal 显示，用户关闭后仍可看 LogsPage
 * - **isStarting**：跟踪过程中的状态，供 StartStopButtons 按钮状态机使用
 *
 * 详见 `PR/06-界面设计.md §3.2 启动停止按钮` 与 `§5.4 日志页`
 */

import { onUnmounted, ref } from "vue";
import { useRouter } from "vue-router";
import { listen, type UnlistenFn } from "@/api";
import { processStart } from "@/api/process";
import { useTaskProgress } from "./useTaskProgress";
import { useToast } from "./useToast";

/** process_crashed 事件 payload（对应后端 serde_json::json!） */
export interface ProcessCrashedEvent {
  exit_code: number | null;
  stderr_tail: string[];
  /** 触发原因：early_exit / health_check_detected / monitor_detected */
  reason: "early_exit" | "health_check_detected" | "monitor_detected";
}

/** v3.4.2 新增：process_health_warning 事件 payload（启动缓慢提示） */
export interface ProcessHealthWarningEvent {
  /** 已经等待的秒数 */
  elapsed: number;
  /** 预期端口 */
  port?: number;
}

/** 启动回调 */
export interface StartCallbacks {
  /** 任务成功（task_completed，spawn 成功 + 早期检测通过） */
  onComplete?: (summary: string | null) => void;
  /** 任务失败（task_failed，含 stderr tail） */
  onError?: (errorMessage: string | null) => void;
  /** child 死亡（process_crashed 事件，5s~60s 之间） */
  onCrashed?: (event: ProcessCrashedEvent) => void;
}

/**
 * 启动 ComfyUI composable
 *
 * 使用方式：
 * ```ts
 * const { start, progress, message, isStarting, isFailed, errorMessage } = useStartComfyui();
 *
 * async function onClickStart() {
 *   await start({
 *     onCrashed: (e) => console.log('child died', e),
 *   });
 * }
 * ```
 */
export function useStartComfyui() {
  const router = useRouter();
  const toast = useToast();
  const task = useTaskProgress();

  // ========== v3.4.1：本地提交守卫（防止用户连点） ==========
  /**
   * 同步标记"启动请求已提交"。
   *
   * 必须在 `start()` 入口**第一时间**置为 true（任何 await 之前），
   * 这样按钮的 disabled 状态能**同步**反映用户点击，后续连点全部 no-op。
   *
   * 重置时机：
   * - task_completed（成功/失败/取消）→ false
   * - 启动前同步异常 → false
   * - 整个 start() 函数执行完毕 → false（finally 兜底）
   */
  const submitting = ref(false);

  // ========== 启动过程产生的事件监听器（用于 process_crashed 等额外事件） ==========
  const extraUnlisteners: UnlistenFn[] = [];
  let pendingCallbacks: StartCallbacks = {};

  // ========== Actions ==========

  /**
   * 启动 ComfyUI
   *
   * 流程：
   * 1. 调 `processStart()` → 后端提交 task，立即返回 task_id
   * 2. `trackTask(taskId, cb)` → 订阅 task_progress / task_completed
   * 3. `router.push('/logs')` → 用户跳到日志页看实时进度
   * 4. 订阅 `process_crashed` → 5s~60s 期间 child 死亡时弹窗
   * 5. 终态时 toast + 调用回调
   *
   * @returns Promise<taskId> - task 提交后立即 resolve（不等待启动完成）
   */
  async function start(cb?: StartCallbacks): Promise<string> {
    // v3.4.1 防连点：第一时间同步置 submitting=true（**任何 await 之前**）
    // 按钮的 currentState 计算属性会同步看到 submitting.value === true，
    // 立即把按钮置灰，后续连点全部 no-op。
    //
    // 兼容：StartStopButtons.onStart 入口会先调 markSubmitting()（onStart 入口预置），
    // 此时 submitting 已为 true，幂等继续不报错。
    submitting.value = true;
    pendingCallbacks = cb ?? {};

    try {
      // 1. 提交 task
      let taskId: string;
      try {
        taskId = await processStart();
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        toast.error("启动失败", msg);
        pendingCallbacks.onError?.(msg);
        throw e;
      }

      // 2. 订阅 process_crashed + process_health_warning
      // - process_crashed：5s~60s 期间 child 死亡（task_completed 之前）
      // - process_health_warning：每 30s 推一次（启动缓慢提示）
      await setupExtraListeners();

      // 3. 跟踪 task 进度
      // v3.4.1：在终态时同步重置 submitting（success/failed/cancelled 三个分支都覆盖）
      let terminalEventFired = false;
      await task.trackTask(taskId, {
        onComplete: (summary) => {
          terminalEventFired = true;
          submitting.value = false;
          const content = summary
            ? `启动成功：${summary}`
            : "启动成功：ComfyUI 已提交启动命令，等待 health_check";
          toast.success(content);
          pendingCallbacks.onComplete?.(summary);
        },
        onError: (errorMsg) => {
          // task_failed 时 errorMsg 包含完整 stderr tail（来自 ProcessError::EarlyExit Display）
          terminalEventFired = true;
          submitting.value = false;
          toast.error("启动失败", errorMsg ?? "未知错误");
          pendingCallbacks.onError?.(errorMsg);
        },
      });

      // 4. 兜底：万一 trackTask 内部没触发 onComplete/onError 也没拒绝
      // （例如 task 在另一处被取消 / TaskScheduler 异常），确保 submitting 一定被重置
      if (!terminalEventFired) {
        submitting.value = false;
      }

      // 5. 跳转到 /logs（让用户看到实时进度 + 日志）
      if (router.currentRoute.value.path !== "/logs") {
        void router.push("/logs");
      }

      return taskId;
    } finally {
      // 兜底：保证 submitting 一定被重置
      submitting.value = false;
    }
  }

  /**
   * v3.4.2：订阅 process_crashed + process_health_warning 事件
   *
   * 与 task_failed 不同：task_failed 是"task 整体失败"（spawn 后 5s 内 child 死 → EarlyExit），
   * process_crashed 是"task 完成后（spawn 成功）child 又死了"（任意时间点）。
   * 两路都要监听，才能覆盖所有崩溃场景。
   *
   * process_health_warning：每 30s 推一次（启动缓慢时给用户提示）
   */
  async function setupExtraListeners() {
    // 清理旧的
    extraUnlisteners.forEach((un) => un());
    extraUnlisteners.length = 0;

    // process_crashed：child 死亡
    const unCrashed = await listen<ProcessCrashedEvent>("process_crashed", (e) => {
      const { exit_code, stderr_tail, reason } = e.payload;
      const tailText = stderr_tail.join("\n");
      const reasonLabel =
        reason === "early_exit"
          ? "早期退出（5s 内）"
          : reason === "health_check_detected"
            ? "健康检查发现崩溃"
            : "运行中崩溃（monitor 检测）";
      const errorMsg = `ComfyUI ${reasonLabel}（exit code: ${exit_code ?? "未知"}）\n\n${tailText}`;

      toast.error("ComfyUI 进程已崩溃", errorMsg);
      pendingCallbacks.onCrashed?.(e.payload);

      // 跳到 /logs 让用户看完整日志流
      if (router.currentRoute.value.path !== "/logs") {
        void router.push("/logs");
      }
    });
    extraUnlisteners.push(unCrashed);

    // v3.4.2 新增：process_health_warning（启动缓慢提示）
    // - 后端 health_check 每 30s 推一次（30s / 60s / 90s / ...）
    // - 显示 toast：让用户知道 ComfyUI 还在启动中，只是慢
    // - 不跳转页面（用户在 LaunchPage 时可能正在配置参数）
    const unWarning = await listen<ProcessHealthWarningEvent>(
      "process_health_warning",
      (e) => {
        const { elapsed } = e.payload;
        const minutes = Math.floor(elapsed / 60);
        const seconds = elapsed % 60;
        const elapsedStr = minutes > 0
          ? `${minutes}分${seconds}秒`
          : `${seconds}秒`;
        toast.warn(
          `ComfyUI 已启动 ${elapsedStr} 仍未就绪，可能正在加载模型或初始化 GPU...`,
        );
      },
    );
    extraUnlisteners.push(unWarning);
  }

  /** 重置 composable 状态（页面切换/手动重置时用） */
  function reset() {
    submitting.value = false;
    task.reset();
    extraUnlisteners.forEach((un) => un());
    extraUnlisteners.length = 0;
    pendingCallbacks = {};
  }

  // ========== v3.4.1 外部守卫接口 ==========

  /**
   * 同步标记 submitting=true（供 StartStopButtons 在 onStart 入口调用）
   *
   * 设计意图：
   * - onStart 在调 `start()` 之前需要先 markSubmitting（**任何 await 之前**），
   *   这样 Vue 反应式能立刻更新按钮状态，后续连点被 disabled 拦截
   * - 如果不预置 submitting，从 onStart 到 start() 内的第一行 set submitting
   *   之间有窗口（precheck await），用户可能在此期间连点
   *
   * 注意：
   * - 调用方需保证在 onStart 入口第一行调用（在任何 await / 同步检查前）
   * - 后续在终态回调（onComplete/onError）或提前 return 时调用 `unmarkSubmitting` 重置
   */
  function markSubmitting() {
    submitting.value = true;
  }

  /**
   * 同步重置 submitting=false（供 StartStopButtons 在守卫失败时调用）
   *
   * 使用场景：
   * - onStart 进入时已 markSubmitting，但守卫 1/2/3 不通过需 return
   * - 此时需要 unmarkSubmitting 解除按钮 disabled 状态
   */
  function unmarkSubmitting() {
    submitting.value = false;
  }

  // ========== 生命周期 ==========
  onUnmounted(() => {
    extraUnlisteners.forEach((un) => un());
    extraUnlisteners.length = 0;
  });

  return {
    // state（从 useTaskProgress 透传）
    progress: task.progress,
    message: task.message,
    status: task.status,
    isRunning: task.isRunning,
    isCompleted: task.isCompleted,
    isFailed: task.isFailed,
    currentTaskId: task.currentTaskId,
    // v3.4.1 新增：本地提交守卫（同步防连点）
    submitting,
    // actions
    start,
    reset,
    // v3.4.1 新增：外部守卫辅助（StartStopButtons 在 onStart 入口调用）
    markSubmitting,
    unmarkSubmitting,
  };
}
