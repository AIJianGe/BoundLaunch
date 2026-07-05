/**
 * useExitConfirm - F24 退出确认对话框 Composable
 *
 * 详见 `PR/06-界面设计.md §7.8 窗口与托盘行为`。
 *
 * 行为：
 * - 调用 `processStatus()` 判断 ComfyUI 状态
 * - 未运行：简确认「⚠ 退出 无界启动器？」+ [取消] [退出]
 * - 运行中：详细确认（含 PID/端口/运行时长警告文案）+ [取消] [停止并退出]
 * - 返回 Promise<boolean>：true=确认退出，false=取消
 *
 * 复用 Naive UI `useDialog` 弹模态框。
 */

import { computed, ref } from "vue";
import { useDialog, useMessage } from "naive-ui";
import { processStatus } from "@/api/process";
import { useProcessStore } from "@/stores/process";
import { devLog } from "./useDevLog";
import type { ProcessStatus } from "@/api/types";

export interface ExitConfirmOptions {
  /** 强制走详细确认（即使 ComfyUI 未运行） */
  forceDetail?: boolean;
}

export function useExitConfirm() {
  const dialog = useDialog();
  const message = useMessage();
  const processStore = useProcessStore();

  /** 当前 ComfyUI 状态（实时查询） */
  const currentStatus = ref<ProcessStatus | null>(null);

  /** 详细确认场景：派生 PID / 端口 / 运行时长 */
  const runningDetail = computed(() => {
    const st = currentStatus.value;
    if (!st || st.kind !== "running") return null;
    const startedAt = new Date(st.started_at);
    const elapsedMs = Date.now() - startedAt.getTime();
    return {
      pid: st.pid,
      port: st.port,
      elapsedMs,
      elapsedText: formatDuration(elapsedMs),
    };
  });

  /**
   * 弹退出确认对话框
   *
   * @returns Promise<boolean> true=确认退出，false=取消
   */
  async function confirmExit(
    options: ExitConfirmOptions = {},
  ): Promise<boolean> {
    devLog("[useExitConfirm]", "enter", { options });
    try {
      currentStatus.value = await processStatus();
      devLog("[useExitConfirm]", "processStatus_ok", { status: currentStatus.value });
    } catch (e) {
      devLog("[useExitConfirm]", "processStatus_failed", { msg: String(e) });
      console.error("[useExitConfirm] processStatus failed", e);
      // 查询失败时按未运行处理（兜底）
      currentStatus.value = { kind: "stopped" };
    }

    const isRunning =
      currentStatus.value?.kind === "running" ||
      currentStatus.value?.kind === "starting";
    devLog("[useExitConfirm]", "decision", { isRunning, forceDetail: options.forceDetail });

    return new Promise<boolean>((resolve) => {
      if (isRunning || options.forceDetail) {
        // 详细确认（含 PID/端口/运行时长）
        const detail = runningDetail.value;
        const pidText = detail ? `• 进程 PID: ${detail.pid}\n` : "";
        const portText = detail ? `• 监听端口: ${detail.port}\n` : "";
        const elapsedText = detail ? `• 运行时长: ${detail.elapsedText}\n` : "";
        const statusText = isRunning
          ? `ComfyUI 正在运行：\n${pidText}${portText}${elapsedText}`
          : "ComfyUI 状态查询失败：\n";

        devLog("[useExitConfirm]", "dialog_warning_detail", { statusText });
        dialog.warning({
          title: "⚠ 退出 无界启动器？",
          content: `${statusText}\n退出后 ComfyUI 进程组将一并停止，\n所有未完成的生成任务会中断。`,
          positiveText: "停止并退出",
          negativeText: "取消",
          onPositiveClick: () => {
            devLog("[useExitConfirm]", "click_positive", {});
            resolve(true);
          },
          onNegativeClick: () => {
            devLog("[useExitConfirm]", "click_negative", {});
            resolve(false);
          },
          onClose: () => {
            devLog("[useExitConfirm]", "dialog_close", {});
            resolve(false);
          },
        });
      } else {
        // 简确认（未运行）
        devLog("[useExitConfirm]", "dialog_warning_simple", {});
        dialog.warning({
          title: "⚠ 退出 无界启动器？",
          content: "确认退出 launcher 吗？",
          positiveText: "退出",
          negativeText: "取消",
          onPositiveClick: () => {
            devLog("[useExitConfirm]", "click_positive", {});
            resolve(true);
          },
          onNegativeClick: () => {
            devLog("[useExitConfirm]", "click_negative", {});
            resolve(false);
          },
          onClose: () => {
            devLog("[useExitConfirm]", "dialog_close", {});
            resolve(false);
          },
        });
      }
    });
  }

  return {
    confirmExit,
    currentStatus,
    runningDetail,
  };
}

/** 格式化毫秒为可读时长（1h 23m / 12m 34s / 56s） */
function formatDuration(ms: number): string {
  const totalSec = Math.floor(ms / 1000);
  const hours = Math.floor(totalSec / 3600);
  const minutes = Math.floor((totalSec % 3600) / 60);
  const seconds = totalSec % 60;

  if (hours > 0) {
    return `${hours}h ${minutes}m`;
  } else if (minutes > 0) {
    return `${minutes}m ${seconds}s`;
  } else {
    return `${seconds}s`;
  }
}
