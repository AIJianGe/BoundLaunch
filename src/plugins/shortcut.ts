/**
 * shortcut - 全局快捷键插件
 *
 * 详见 `PR/06-界面设计.md §7.9 快捷键`
 *
 * 实现方式：window 级 keydown 监听
 *
 * 注：设计文档提到「在 globalShortcut 模块注册」，
 * 但 tauri-plugin-global-shortcut 适用于窗口失焦时的全局快捷键（如媒体键）。
 * 本期快捷键仅在 launcher 窗口聚焦时生效，使用 window keydown 即可满足需求。
 * 二期接入 globalShortcut 后可替换。
 *
 * 快捷键清单（与设计文档一致）：
 * | 快捷键 | 功能 |
 * |---|---|
 * | Ctrl+Enter | 启动 ComfyUI（启动页） |
 * | Ctrl+Shift+Enter | 停止 ComfyUI（启动页） |
 * | Ctrl+R | 刷新环境探查（启动页） |
 * | Ctrl+L | 切换到日志页 |
 * | Ctrl+, | 切换到设置页 |
 * | Ctrl+Q | **F24 退出 launcher（弹确认 + 联动关闭 ComfyUI）** |
 * | Ctrl+1 ~ Ctrl+8 | 切换到对应页面 |
 * | Ctrl+F | 焦点搜索框（日志页/插件管理页） |
 * | F5 | 刷新当前页面（重新加载路由） |
 * | Esc | 关闭弹窗 / 取消选中（默认行为） |
 * | Ctrl+Shift+I | 打开 DevTools（仅开发模式） |
 *
 * 设计模式：
 * - **Strategy**：不同快捷键不同处理函数
 * - **Command**：每个快捷键封装为一个 Command 对象
 *
 * 使用方式：
 * ```ts
 * // App.vue setup
 * const shortcut = useShortcuts();
 * onUnmounted(() => shortcut.cleanup());
 * ```
 */

import { onMounted, onUnmounted } from "vue";
import { useRouter } from "vue-router";
import { useProcessStore } from "@/stores/process";
import { useEnvStore } from "@/stores/env";
import { useShutdown } from "@/composables/useShutdown";

/** 主导航路径，对应 Ctrl+1 ~ Ctrl+7（v3.x：删除"模型路径"页后减少到 7 个） */
const NAV_PATHS = [
  "/launch",
  "/core",
  "/plugins",
  "/settings",
  "/logs",
  "/tasks",
  "/about",
] as const;

const SEARCHABLE_ROUTES = new Set(["logs", "plugins"]);

export function useShortcuts() {
  const router = useRouter();
  const processStore = useProcessStore();
  const envStore = useEnvStore();
  const shutdown = useShutdown();

  /** Ctrl+Enter：启动 ComfyUI（仅在启动页生效） */
  function onCtrlEnter() {
    if (router.currentRoute.value.path !== "/launch") return;
    if (processStore.isAlive) return;
    processStore.start().catch((e) => console.warn("[shortcut] start failed:", e));
  }

  /** Ctrl+Shift+Enter：停止 ComfyUI */
  function onCtrlShiftEnter() {
    if (!processStore.isAlive) return;
    processStore.stop().catch((e) => console.warn("[shortcut] stop failed:", e));
  }

  /** Ctrl+R：刷新环境探查 */
  function onCtrlR(event: KeyboardEvent) {
    if (router.currentRoute.value.path !== "/launch") return;
    event.preventDefault();
    envStore.invalidateCache().catch((e) => console.warn("[shortcut] refresh env:", e));
  }

  /** Ctrl+L：切换到日志页 */
  function onCtrlL(event: KeyboardEvent) {
    event.preventDefault();
    router.push("/logs");
  }

  /** Ctrl+,：切换到设置页 */
  function onCtrlComma(event: KeyboardEvent) {
    event.preventDefault();
    router.push("/settings");
  }

  /** Ctrl+1 ~ Ctrl+8：切换到对应页面 */
  function onCtrlDigit(digit: number, event: KeyboardEvent) {
    if (digit < 1 || digit > NAV_PATHS.length) return;
    event.preventDefault();
    router.push(NAV_PATHS[digit - 1]);
  }

  /** Ctrl+F：焦点搜索框（日志页 / 插件管理页） */
  function onCtrlF(event: KeyboardEvent) {
    const routeName = router.currentRoute.value.name;
    if (typeof routeName !== "string" || !SEARCHABLE_ROUTES.has(routeName)) return;
    event.preventDefault();
    // 通过自定义事件通知页面（页面在 onMounted 时注册监听）
    window.dispatchEvent(new CustomEvent("shortcut:focus-search"));
  }

  /** Ctrl+Shift+I：打开 DevTools（仅开发模式） */
  function onCtrlShiftI(event: KeyboardEvent) {
    if (!import.meta.env.DEV) return;
    event.preventDefault();
    // Tauri 2 在开发模式下默认开启 DevTools（F12）
    // 这里通过自定义事件通知（main.ts 中可监听并调用 webview.open_devtools）
    window.dispatchEvent(new CustomEvent("shortcut:toggle-devtools"));
  }

  /** Ctrl+Q：F24 退出 launcher（弹确认 + 联动关闭 ComfyUI） */
  function onCtrlQ(event: KeyboardEvent) {
    event.preventDefault();
    shutdown.requestExit("shortcut_ctrl_q").catch((e) =>
      console.warn("[shortcut] shutdown failed:", e),
    );
  }

  /** F5：刷新当前页面 */
  function onF5(event: KeyboardEvent) {
    event.preventDefault();
    // Vue Router 不重新渲染相同组件，使用 location.reload 强制刷新
    // 或通过 emit 通知当前页面
    window.dispatchEvent(new CustomEvent("shortcut:refresh-page"));
  }

  function handler(event: KeyboardEvent) {
    // 忽略输入框中的快捷键（除了 Esc 和 F5）
    const target = event.target as HTMLElement;
    const isInput =
      target?.tagName === "INPUT" ||
      target?.tagName === "TEXTAREA" ||
      target?.isContentEditable;

    if (isInput) {
      // 仅放行 Esc
      if (event.key !== "Escape") return;
    }

    const ctrl = event.ctrlKey || event.metaKey; // macOS 用 Cmd

    // Ctrl+Shift+Enter
    if (ctrl && event.shiftKey && event.key === "Enter") {
      onCtrlShiftEnter();
      return;
    }

    // Ctrl+Shift+I（DevTools）
    if (ctrl && event.shiftKey && (event.key === "I" || event.key === "i")) {
      onCtrlShiftI(event);
      return;
    }

    // Ctrl+Enter
    if (ctrl && !event.shiftKey && event.key === "Enter") {
      onCtrlEnter();
      return;
    }

    // Ctrl+R
    if (ctrl && !event.shiftKey && (event.key === "r" || event.key === "R")) {
      onCtrlR(event);
      return;
    }

    // Ctrl+L
    if (ctrl && !event.shiftKey && (event.key === "l" || event.key === "L")) {
      onCtrlL(event);
      return;
    }

    // Ctrl+,
    if (ctrl && event.key === ",") {
      onCtrlComma(event);
      return;
    }

    // Ctrl+1 ~ Ctrl+7
    if (ctrl && /^[1-7]$/.test(event.key)) {
      onCtrlDigit(parseInt(event.key), event);
      return;
    }

    // Ctrl+F
    if (ctrl && (event.key === "f" || event.key === "F")) {
      onCtrlF(event);
      return;
    }

    // Ctrl+Q（F24 退出 launcher）
    if (ctrl && !event.shiftKey && (event.key === "q" || event.key === "Q")) {
      onCtrlQ(event);
      return;
    }

    // F5
    if (event.key === "F5") {
      onF5(event);
      return;
    }
  }

  onMounted(() => {
    window.addEventListener("keydown", handler);
  });

  function cleanup() {
    window.removeEventListener("keydown", handler);
  }

  onUnmounted(cleanup);

  return { cleanup };
}
