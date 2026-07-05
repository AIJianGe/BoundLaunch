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
import { useConfigStore } from "@/stores/config";
import { useToast } from "@/composables/useToast";
import { useShutdown } from "@/composables/useShutdown";
import { devLog } from "@/composables/useDevLog";
import { listen, type UnlistenFn } from "@/api";

/** 托盘动作事件 payload */
interface TrayActionPayload {
  action: "start" | "stop" | "show" | "quit";
}

export function useTray() {
  const processStore = useProcessStore();
  const configStore = useConfigStore();
  const toast = useToast();
  const shutdown = useShutdown();
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

  /**
   * F24 退出 launcher
   *
   * 通过 `useShutdown.requestExit('tray_quit')` 走完整 F24 流程：
   * 1. 弹确认对话框（未运行/运行中两种形态）
   * 2. 调 `shutdown_all` Tauri command
   * 3. 后端 ShutdownCoordinator 5 步事务
   * 4. app.exit(0)
   *
   * 不再直接 `app.exit()` 避免 python worker 残留。
   */
  async function quitApp() {
    await shutdown.requestExit("tray_quit");
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

  /**
   * 监听窗口关闭按钮
   *
   * F19b + F24 行为：
   * - minimize_to_tray 开关=true 且 ComfyUI 在运行：保活 ComfyUI，仅隐藏窗口
   * - 其他情况：弹确认对话框 → 走 F24 shutdown 流程
   *
   * 注：Tauri 2 `onCloseRequested` 回调必须是同步签名：
   * 1) 同步 `preventDefault()`
   * 2) 副作用延后到 `queueMicrotask`（避免与 Tauri 默认销毁流程 race）
   */
  async function setupCloseToTray() {
    console.log("[tray] setupCloseToTray called");
    devLog("[tray]", "setup_enter", {});
    try {
      const win = getCurrentWindow();
      console.log("[tray] got current window, label =", win.label);
      devLog("[tray]", "got_window", { label: win.label });

      const unlisten = await win.onCloseRequested((event) => {
        // F19b：读用户设置（默认 minimize_to_tray=false → 走 F24）
        const minimizeToTray = configStore.config?.ui?.minimize_to_tray ?? false;
        const isAlive = processStore.isAlive;
        const cfgUi = configStore.config?.ui;
        console.log(
          "[tray] close-requested event, minimize_to_tray =",
          minimizeToTray,
          ", isAlive =",
          isAlive,
        );
        devLog("[tray]", "handler_enter", {
          minimizeToTray,
          isAlive,
          configUiExists: cfgUi !== undefined,
          configUiRaw: cfgUi,
        });

        // 阻止默认关闭（无论走哪条路径都要拦截，因为要再走自定义逻辑）
        event.preventDefault();
        devLog("[tray]", "preventDefault_called", {});

        // 副作用延后到下一个微任务
        queueMicrotask(async () => {
          devLog("[tray]", "microtask_enter", { minimizeToTray, isAlive });
          // F19b 仅在 ComfyUI 在运行时生效：避免"未运行却最小化"的反直觉行为
          // （minimizeToTray=true 但 ComfyUI 未运行 → 强制走 F24 弹确认关闭）
          if (minimizeToTray && isAlive) {
            devLog("[tray]", "decision_f19b", { action: "win.hide" });
            // F19b 行为：仅隐藏窗口，ComfyUI 继续运行
            try {
              await win.hide();
              devLog("[tray]", "action_done", { action: "win.hide", success: true });
              toast.info("ComfyUI 仍在运行，已最小化到托盘");
            } catch (e) {
              devLog("[tray]", "error", { stage: "win.hide", msg: String(e) });
              console.warn("[tray] hide failed:", e);
            }
          } else {
            devLog("[tray]", "decision_f24", { action: "shutdown.requestExit" });
            // F24 行为：弹确认 → shutdown_all → app.exit
            try {
              await shutdown.requestExit("window_close");
              devLog("[tray]", "action_done", { action: "shutdown.requestExit", success: true });
            } catch (e) {
              devLog("[tray]", "error", { stage: "shutdown.requestExit", msg: String(e) });
              console.error("[tray] shutdown failed:", e);
            }
          }
        });
      });
      unlisteners.push(unlisten);
      console.log("[tray] onCloseRequested registered, total unlisteners =", unlisteners.length);
      devLog("[tray]", "setup_done", { unlistenerCount: unlisteners.length });
    } catch (e) {
      devLog("[tray]", "error", { stage: "setupCloseToTray", msg: String(e) });
      console.error("[tray] setup close-to-tray failed:", e);
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
