/**
 * tray - 系统托盘插件
 *
 * 详见 `PR/06-界面设计.md §7.8 窗口与托盘行为`
 *
 * 架构：
 * - Rust 端（src-tauri/src/tray.rs）：创建 TrayIcon + 菜单 + emit("tray_action")
 * - 前端（本文件）：监听 tray_action 事件，分发到 processStore / router
 *
 * 托盘右键菜单：
 * - ▶ 启动 ComfyUI → emit("tray_action", { action: "start" })
 * - ⏹ 停止 ComfyUI → emit("tray_action", { action: "stop" })
 * - ─────────────
 * - 📋 显示主窗口 → emit("tray_action", { action: "show" })
 * - 🚪 退出 → emit("tray_action", { action: "quit" })
 *
 * 双击托盘图标：Rust 端直接调用 window.show() + set_focus()，不经过前端
 *
 * 图标状态：
 * - 未运行：灰色（默认 launcher 图标）
 * - 启动中：黄色
 * - 运行中：绿色
 * - 异常：红色
 *
 * 注：当前 Rust 端使用默认窗口图标，未根据进程状态切换。
 *     状态切换需后续在 Rust 端监听 process 事件并调用 tray.set_icon()。
 *     详见 PR/03-模块设计/06-ProcessLauncher.md §10 崩溃恢复
 *
 * 设计模式：
 * - **Observer**：listen 后端 tray_action 事件
 * - **Command**：每个 action 对应一个处理函数
 *
 * 使用方式：
 * ```ts
 * // App.vue setup
 * const tray = useTray();
 * onUnmounted(() => tray.cleanup());
 * ```
 */

import { onMounted, onUnmounted } from "vue";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useProcessStore } from "@/stores/process";
import { useToast } from "@/composables/useToast";
import { listen, type UnlistenFn } from "@/api";

/** 托盘动作事件 payload */
interface TrayActionPayload {
  action: "start" | "stop" | "show" | "quit";
}

export function useTray() {
  const processStore = useProcessStore();
  const toast = useToast();
  const unlisteners: UnlistenFn[] = [];

  /** 显示主窗口 */
  async function showMainWindow() {
    try {
      const win = getCurrentWindow();
      await win.show();
      await win.setFocus();
      await win.unminimize();
    } catch (e) {
      console.warn("[tray] show window failed:", e);
    }
  }

  /** 退出应用 */
  async function quitApp() {
    try {
      // 提示用户：若 ComfyUI 运行中，会一并终止
      if (processStore.isAlive) {
        toast.info("正在停止 ComfyUI 并退出...");
        await processStore.stop().catch((e) => {
          console.warn("[tray] stop on quit failed:", e);
        });
      }
      const win = getCurrentWindow();
      await win.destroy();
    } catch (e) {
      console.error("[tray] quit failed:", e);
      // 强制退出
      window.close();
    }
  }

  /** 处理托盘动作 */
  async function handleAction(action: TrayActionPayload["action"]) {
    switch (action) {
      case "start":
        if (!processStore.isAlive) {
          try {
            await processStore.start();
            toast.success("ComfyUI 启动中");
          } catch (e) {
            toast.error("启动失败", e);
          }
        }
        break;
      case "stop":
        if (processStore.isAlive) {
          try {
            await processStore.stop();
            toast.success("ComfyUI 已停止");
          } catch (e) {
            toast.error("停止失败", e);
          }
        }
        break;
      case "show":
        await showMainWindow();
        break;
      case "quit":
        await quitApp();
        break;
      default:
        console.warn("[tray] unknown action:", action);
    }
  }

  /** 监听窗口关闭按钮：最小化到托盘（保活 ComfyUI 进程） */
  async function setupCloseToTray() {
    try {
      const win = getCurrentWindow();
      await win.onCloseRequested(async (event) => {
        if (processStore.isAlive) {
          // 阻止默认关闭，最小化到托盘
          event.preventDefault();
          await win.hide();
          toast.info("ComfyUI 仍在运行，已最小化到托盘");
        }
        // 否则正常关闭
      });
    } catch (e) {
      console.warn("[tray] setup close-to-tray failed:", e);
    }
  }

  /** 订阅托盘动作事件 */
  async function subscribe() {
    unlisteners.push(
      await listen<TrayActionPayload>("tray_action", (e) => {
        handleAction(e.payload.action).catch((err) => {
          console.error("[tray] handle action failed:", err);
        });
      }),
    );
  }

  function unsubscribe() {
    unlisteners.forEach((un) => un());
    unlisteners.length = 0;
  }

  onMounted(async () => {
    await subscribe();
    await setupCloseToTray();
  });

  function cleanup() {
    unsubscribe();
  }

  onUnmounted(cleanup);

  return { cleanup };
}
