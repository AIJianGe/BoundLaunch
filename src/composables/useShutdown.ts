/**
 * useShutdown - F24 退出流程 Composable
 *
 * 详见 `PR/06-界面设计.md §7.8` 与 `PR/03-模块设计/06-ProcessLauncher.md §12`。
 *
 * 三路径统一（窗口 [X] / 托盘「退出」/ Ctrl+Q）：
 * 1. `requestExit(reason)` → 弹确认对话框 → 调 `shutdownAll(reason)`
 * 2. shutdown 期间显示进度模态框
 * 3. 30s 超时兜底（理论上后端 `ShutdownCoordinator` 已兜底）
 *
 * 设计模式：
 * - **Facade**：封装 `useExitConfirm` + `shutdownAll` + 进度模态框
 * - **Template Method**：5 步事务在 `ShutdownCoordinator` 中执行，前端只负责 3 步（确认→等待→退出）
 */

import { ref } from "vue";
import { useMessage } from "naive-ui";
import type { MessageReactive } from "naive-ui";
import { shutdownAll } from "@/api/process";
import { useExitConfirm } from "./useExitConfirm";
import { useProcessStore } from "@/stores/process";
import { devLog } from "./useDevLog";
import type { ShutdownReason } from "@/api/types";

/** F24 后端超时上限（与 ShutdownCoordinator::SHUTDOWN_TIMEOUT 一致） */
const SHUTDOWN_TIMEOUT_MS = 30_000;

export function useShutdown() {
  const message = useMessage();
  const { confirmExit } = useExitConfirm();
  const processStore = useProcessStore();

  /**
   * 是否处于退出中（用于按钮 disabled）
   *
   * 由 processStore.isExiting 暴露，前端 watch 同步
   */
  const isExiting = ref(processStore.isExiting);

  /**
   * 触发退出流程
   *
   * @param reason 退出原因（[window_close / tray_quit / shortcut_ctrl_q / restart]）
   * @returns Promise<boolean> true=已确认并提交，false=取消
   */
  async function requestExit(reason: ShutdownReason): Promise<boolean> {
    devLog("[useShutdown]", "enter", { reason, isExiting: processStore.isExiting });
    // 已处于退出中：直接返回（防重入）
    if (processStore.isExiting) {
      console.warn("[useShutdown] already exiting, skip");
      devLog("[useShutdown]", "skip_already_exiting", {});
      return false;
    }

    // 步骤 1：弹确认对话框
    devLog("[useShutdown]", "before_confirm", {});
    const confirmed = await confirmExit();
    devLog("[useShutdown]", "after_confirm", { confirmed });
    if (!confirmed) {
      return false;
    }

    // 步骤 2：标记退出中（启动页按钮置灰）
    processStore.setExiting(true);

    // 步骤 3：显示进度消息（MessageReactive 拥有 destroy() 方法）
    let progressReactive: MessageReactive | null = null;
    let timeoutHandle: ReturnType<typeof setTimeout> | null = null;

    try {
      progressReactive = message.loading("正在停止 ComfyUI...", {
        duration: 0,
        closable: false,
      });

      // 步骤 4：调用 shutdown_all（Promise.race 加 30s 兜底）
      const shutdownPromise = shutdownAll(reason);
      const timeoutPromise = new Promise<never>((_, reject) => {
        timeoutHandle = setTimeout(() => {
          reject(new Error(`shutdown_all timed out after ${SHUTDOWN_TIMEOUT_MS}ms`));
        }, SHUTDOWN_TIMEOUT_MS);
      });

      try {
        const report = await Promise.race([shutdownPromise, timeoutPromise]);
        console.info("[useShutdown] shutdown_all completed", report);
        // 后端 ShutdownCoordinator 成功后会自动 app.exit(0)
        // 正常情况下我们走不到这里（进程已退出）
        // 但如果 30s 内后端没退出，前端走到这里后 force reload
        forceReload();
      } catch (e) {
        // 超时或后端错误：前端兜底刷新（让 Tauri 主进程决定）
        console.error("[useShutdown] shutdown failed", e);
        // 显示错误但仍 force reload（避免卡死）
        message.error("退出流程异常，正在强制刷新...");
        setTimeout(() => forceReload(), 1500);
      } finally {
        if (timeoutHandle) clearTimeout(timeoutHandle);
      }
    } finally {
      if (progressReactive) progressReactive.destroy();
    }

    return true;
  }

  /**
   * 强制重载（兜底）
   *
   * 当 30s 超时或后端退出异常时，前端调用此函数再次触发退出。
   * 注：正常流程下后端 ShutdownCoordinator 成功后会调 `app.exit(0)`，
   * 进程已终止，前端不会执行到这里。
   */
  function forceReload() {
    console.warn("[useShutdown] force reload (fallback exit)");
    // Tauri 2：调 getCurrentWindow().close()（前端兜底）
    try {
      const { getCurrentWindow } = require("@tauri-apps/api/window");
      getCurrentWindow().close();
    } catch (e) {
      console.error("[useShutdown] force reload failed", e);
      // 终极兜底：刷新页面
      location.reload();
    }
  }

  return {
    isExiting,
    requestExit,
  };
}
