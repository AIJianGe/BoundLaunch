/**
 * crashRecovery - 崩溃恢复插件
 *
 * 详见 `PR/06-界面设计.md §7.10 崩溃恢复提示`
 *
 * 流程：
 * 1. 后端启动时 check_stale_process 检测遗留 ComfyUI 进程
 * 2. 检测到 → emit("stale_process_detected", { pid, started_at, args })
 * 3. processStore 接收事件，写入 staleProcess 状态
 * 4. useCrashRecovery 监听 staleProcess 变化，弹出确认对话框
 * 5. 用户选择：
 *    - 「终止遗留进程」→ processStore.killStale(pid)
 *    - 「保留」→ processStore.dismissStale()（清理 PID 文件，不终止进程）
 *
 * 30 秒自动终止（避免用户离开时卡住）：
 * - 弹窗显示后启动 30s 倒计时
 * - 用户未操作 → 自动选择「终止」
 *
 * 设计模式：
 * - **Observer**：watch processStore.staleProcess 变化
 * - **State**：dialog 可见性 + 倒计时状态
 *
 * 使用方式（必须在 NDialogProvider 内的 setup 上下文中调用）：
 * ```ts
 * // App.vue
 * const crashRecovery = useCrashRecovery(); // 自动注册监听
 * onUnmounted(() => crashRecovery.cleanup());
 * ```
 */

import { ref, watch, onUnmounted } from "vue";
import { useProcessStore } from "@/stores/process";
import { useConfirm } from "@/composables/useConfirm";
import { useToast } from "@/composables/useToast";

/** 自动终止倒计时（毫秒） */
const AUTO_KILL_TIMEOUT_MS = 30_000;

export function useCrashRecovery() {
  const processStore = useProcessStore();
  const confirm = useConfirm();
  const toast = useToast();

  const countdown = ref(0);
  const isHandling = ref(false);
  let countdownTimer: ReturnType<typeof setInterval> | null = null;

  /**
   * 显示确认对话框
   *
   * 返回 true=用户选择终止 / false=用户选择保留
   */
  async function showStaleDialog(pid: number, startedAt: string): Promise<boolean> {
    const content = [
      `上次启动时间：${startedAt}`,
      `进程 PID：${pid}`,
      "",
      "可能原因：",
      "• launcher 上次异常退出",
      "• 系统强制关机",
      "• ComfyUI 卡死被强制结束",
      "",
      `ℹ 保留可能导致端口冲突，${countdown.value}s 后将自动终止`,
    ].join("\n");

    return confirm.confirm({
      title: "⚠️ 检测到遗留的 ComfyUI 进程",
      content,
      type: "warning",
      positiveText: "终止遗留进程",
      negativeText: "保留",
      maskClosable: false,
    });
  }

  /** 启动 30s 自动终止倒计时 */
  function startCountdown(onTimeout: () => void) {
    countdown.value = Math.floor(AUTO_KILL_TIMEOUT_MS / 1000);
    countdownTimer = setInterval(() => {
      countdown.value -= 1;
      if (countdown.value <= 0) {
        stopCountdown();
        onTimeout();
      }
    }, 1000);
  }

  function stopCountdown() {
    if (countdownTimer) {
      clearInterval(countdownTimer);
      countdownTimer = null;
    }
  }

  async function handleStale(pid: number, startedAt: string) {
    if (isHandling.value) return;
    isHandling.value = true;

    let userChoice = false;
    let timedOut = false;

    // 启动自动终止倒计时
    const timeoutPromise = new Promise<boolean>((resolve) => {
      startCountdown(() => {
        timedOut = true;
        resolve(true); // 超时 = 终止
      });
    });

    const userPromise = showStaleDialog(pid, startedAt);

    // 等待用户选择或超时
    userChoice = await Promise.race([userPromise, timeoutPromise]);
    stopCountdown();

    if (timedOut) {
      toast.warn("30 秒未响应，自动终止遗留进程");
    }

    if (userChoice) {
      // 用户选择「终止」或超时
      try {
        await processStore.killStale(pid);
        toast.success("已终止遗留进程");
      } catch (e) {
        toast.error("终止失败", e);
      }
    } else {
      // 用户选择「保留」
      processStore.dismissStale();
      toast.warn("保留遗留进程，可能导致端口冲突");
    }

    isHandling.value = false;
  }

  // 监听 staleProcess 状态
  const stopWatch = watch(
    () => processStore.staleProcess,
    (stale) => {
      if (stale) {
        handleStale(stale.pid, stale.started_at).catch((e) => {
          console.error("[crashRecovery] handle stale failed:", e);
          isHandling.value = false;
          stopCountdown();
        });
      }
    },
  );

  function cleanup() {
    stopWatch();
    stopCountdown();
  }

  onUnmounted(cleanup);

  return { cleanup, countdown };
}
