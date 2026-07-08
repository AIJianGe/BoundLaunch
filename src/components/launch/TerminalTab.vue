<script setup lang="ts">
/**
 * TerminalTab.vue — 伪交互式终端 Tab
 *
 * 设计目的：
 * - 跨平台伪终端（Windows: ConPTY, Unix: pseudo_terminal）
 * - 支持多标签页（每个 session 独立 shell）
 * - 启动时自动创建第一个 session
 * - 折叠状态持久化（localStorage）
 * - 窗口 resize 时自适应（防抖 100ms）
 *
 * 数据流：
 * - 用户输入 → term.onData → pty_write 命令 → 后端 pty 子进程
 * - 后端子进程输出 → pty_output 事件 → base64 解码 → term.write
 * - 子进程退出 → pty_exit 事件 → 更新 is_alive 标记
 *
 * 与 LogsPage 的关系：
 * - 不依赖日志数据
 * - 不依赖启动任务
 * - 独立可复用
 */

import { ref, onMounted, onUnmounted, nextTick } from "vue";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { invoke, listen as listenTauri, type UnlistenFn } from "@/api";
import { useToast } from "@/composables/useToast";
import { Plus, X, Terminal as TerminalIcon, ChevronDown, ChevronUp } from "lucide-vue-next";

// ============================================================================
// 类型定义
// ============================================================================

interface PtySize {
  rows: number;
  cols: number;
}

interface TerminalSessionInfo {
  session_id: string;
  shell: string;
  cwd: string;
  size: PtySize;
  is_alive: boolean;
  exit_code: number | null;
  created_at: string;
}

// ============================================================================
// 状态
// ============================================================================

const toast = useToast();
const terminalCollapsed = ref(false);
const terminalContainer = ref<HTMLElement | null>(null);
const activeSessionId = ref<string | null>(null);
const ptySessions = ref<TerminalSessionInfo[]>([]);

let term: Terminal | null = null;
let fitAddon: FitAddon | null = null;
let unlistenPtyOutput: UnlistenFn | null = null;
let unlistenPtyExit: UnlistenFn | null = null;
let resizeTimer: number | null = null;

// ============================================================================
// 折叠状态持久化
// ============================================================================

const COLLAPSED_KEY = "terminal.collapsed";
const storedCollapsed = localStorage.getItem(COLLAPSED_KEY);
if (storedCollapsed !== null) {
  terminalCollapsed.value = storedCollapsed === "true";
}

// ============================================================================
// 工具函数
// ============================================================================

function base64Decode(b64: string): string {
  try {
    const binaryString = atob(b64);
    const bytes = new Uint8Array(binaryString.length);
    for (let i = 0; i < binaryString.length; i++) {
      bytes[i] = binaryString.charCodeAt(i);
    }
    return new TextDecoder().decode(bytes);
  } catch {
    return "";
  }
}

function base64Encode(text: string): string {
  return btoa(unescape(encodeURIComponent(text)));
}

// ============================================================================
// 伪终端事件处理
// ============================================================================

function toggleTerminal() {
  terminalCollapsed.value = !terminalCollapsed.value;
  localStorage.setItem(COLLAPSED_KEY, String(terminalCollapsed.value));
  if (!terminalCollapsed.value) {
    // v3.13：展开后重新布局 + 聚焦
    nextTick(() => {
      if (fitAddon) {
        fitAddon.fit();
      }
      if (term) {
        term.focus();
      }
    });
  }
}

/**
 * v3.13：点击终端区域时强制 focus
 *
 * 在 Tauri WebView 里，xterm 的 canvas 可能因为事件冒泡问题没拿到焦点
 * 这里手动触发 focus 兜底
 */
function onTerminalClick(event: MouseEvent) {
  if (!term) return;
  // 防止冒泡触发 tab 切换
  event.stopPropagation();
  term.focus();
}

/**
 * v3.13：双击全屏
 */
function onTerminalDoubleClick() {
  if (term) {
    term.focus();
  }
}

function handleTerminalResize() {
  if (terminalCollapsed.value) return;
  if (resizeTimer !== null) {
    window.clearTimeout(resizeTimer);
  }
  resizeTimer = window.setTimeout(() => {
    if (fitAddon) {
      fitAddon.fit();
    }
    resizeTimer = null;
  }, 100);
}

// ============================================================================
// 初始化 / 销毁
// ============================================================================

/**
 * v3.13：等待容器有非零尺寸后再初始化 xterm
 *
 * 背景：
 * - TerminalTab 在 LogsPage 的"终端"Tab 内，使用 `display-directive="show"`，初始可能不在激活 Tab
 * - 如果容器当时 `display: none`，getBoundingClientRect() 拿到 width=0、height=0
 * - xterm 在 width=0 时会算出 cols=0/1，导致路径换行（一行只能显示几字符）
 *
 * 解决：用 requestAnimationFrame 轮询直到容器有尺寸，或者用 ResizeObserver 等
 */
function waitForContainer(): Promise<void> {
  return new Promise((resolve) => {
    let frame = 0;
    const MAX_FRAMES = 300; // 5 秒（60fps × 5s）

    const check = () => {
      frame++;
      if (frame > MAX_FRAMES) {
        console.warn("[TerminalTab] waitForContainer timeout, init anyway");
        resolve();
        return;
      }
      if (!terminalContainer.value) {
        requestAnimationFrame(check);
        return;
      }
      const rect = terminalContainer.value.getBoundingClientRect();
      if (rect.width > 0 && rect.height > 0) {
        resolve();
      } else {
        requestAnimationFrame(check);
      }
    };
    check();
  });
}

let resizeObserver: ResizeObserver | null = null;

function initPtyTerminal() {
  if (!terminalContainer.value) return;
  if (term) return; // 防止重复初始化

  term = new Terminal({
    cursorBlink: true,
    fontSize: 12,
    fontFamily: 'Consolas, "Courier New", monospace',
    // 给一个合理的初始尺寸（fitAddon.fit() 之后会被覆盖）
    cols: 80,
    rows: 24,
    theme: {
      background: "#1e1e1e",
      foreground: "#d4d4d4",
      cursor: "#d4d4d4",
      black: "#000000",
      red: "#cd3131",
      green: "#0dbc79",
      yellow: "#e5e510",
      blue: "#2472c8",
      magenta: "#bc3fbc",
      cyan: "#11a8cd",
      white: "#e5e5e5",
      brightBlack: "#666666",
      brightRed: "#f14c4c",
      brightGreen: "#23d18b",
      brightYellow: "#f5f543",
      brightBlue: "#3b8eea",
      brightMagenta: "#d670d6",
      brightCyan: "#29b8db",
      brightWhite: "#ffffff",
    },
    allowProposedApi: true,
  });

  fitAddon = new FitAddon();
  term.loadAddon(fitAddon);

  term.open(terminalContainer.value);
  fitAddon.fit();

  // v3.13：xterm 尺寸变化 → 通知后端 pty 调整（直接复用 onResize）
  term.onResize((size) => {
    if (activeSessionId.value) {
      void invoke("pty_resize", {
        sessionId: activeSessionId.value,
        size: { rows: size.rows, cols: size.cols },
      });
    }
  });

  // 用户输入 → pty_write
  term.onData((data) => {
    if (activeSessionId.value) {
      void invoke("pty_write", {
        sessionId: activeSessionId.value,
        data: base64Encode(data),
      });
    }
  });

  // v3.13：xterm 焦点状态由容器上的 CSS :focus-within 反映（见样式）
  // 旧的 term.onFocus/onBlur 在新版 xterm.js 不再支持

  // v3.13：ResizeObserver 监听容器尺寸变化 → 自动 fit + 通知后端
  // 解决：Tab 切换、窗口缩放、NCard resize 等场景下 xterm 不刷新问题
  resizeObserver = new ResizeObserver(
    debounce(() => {
      if (!term || !fitAddon) return;
      if (terminalCollapsed.value) return;
      fitAddon.fit();
      // 通知后端 pty 调整
      if (activeSessionId.value) {
        void invoke("pty_resize", {
          sessionId: activeSessionId.value,
          size: { rows: term.rows, cols: term.cols },
        });
      }
    }, 100),
  );
  resizeObserver.observe(terminalContainer.value);

  // 监听 window resize（兜底）
  window.addEventListener("resize", handleTerminalResize);
}

/** 简单防抖 */
function debounce<T extends (...args: any[]) => void>(fn: T, ms: number): T {
  let timer: number | null = null;
  return ((...args: any[]) => {
    if (timer !== null) clearTimeout(timer);
    timer = window.setTimeout(() => {
      fn(...args);
      timer = null;
    }, ms);
  }) as T;
}

async function setupPtyListeners() {
  unlistenPtyOutput = await listenTauri<{ session_id: string; data: string }>(
    "pty_output",
    (event) => {
      const payload = event.payload;
      if (payload.session_id === activeSessionId.value && term) {
        const text = base64Decode(payload.data);
        term.write(text);
      }
    },
  );

  unlistenPtyExit = await listenTauri<{ session_id: string; exit_code: number | null }>(
    "pty_exit",
    (event) => {
      const payload = event.payload;
      const session = ptySessions.value.find((s) => s.session_id === payload.session_id);
      if (session) {
        session.is_alive = false;
        session.exit_code = payload.exit_code;
      }
    },
  );
}

async function createPtySession() {
  try {
    const info = await invoke<TerminalSessionInfo>("pty_create_session", {
      shell: null,
      cwd: null,
      size: term
        ? { rows: term.rows, cols: term.cols }
        : { rows: 24, cols: 80 },
    });
    ptySessions.value.push(info);
    activeSessionId.value = info.session_id;
    if (term) {
      term.clear();
      term.focus();
    }
  } catch (e) {
    console.error("Failed to create pty session:", e);
    toast.error("终端创建失败", String(e));
  }
}

async function closePtySession(sessionId: string) {
  try {
    await invoke("pty_close", { sessionId });
    const idx = ptySessions.value.findIndex((s) => s.session_id === sessionId);
    if (idx >= 0) {
      ptySessions.value.splice(idx, 1);
    }
    if (activeSessionId.value === sessionId) {
      activeSessionId.value = ptySessions.value[0]?.session_id ?? null;
    }
  } catch (e) {
    console.error("Failed to close pty session:", e);
  }
}

async function loadPtySessions() {
  try {
    const list = await invoke<TerminalSessionInfo[]>("pty_list_sessions");
    ptySessions.value = list;
    if (!activeSessionId.value && list.length > 0) {
      activeSessionId.value = list[0].session_id;
    }
  } catch (e) {
    console.error("Failed to list pty sessions:", e);
  }
}

// ============================================================================
// 生命周期
// ============================================================================

/**
 * v3.13：强化焦点获取
 *
 * 问题：xterm 初始化后不会自动获取焦点，键盘事件被其他元素拦截
 * 解决：
 * 1. onMounted 后 200ms force focus
 * 2. 之后每 200ms 尝试一次，连续 5 次（共 1 秒）
 * 3. 用户点击终端区域时立即 focus
 */
function tryFocusTerminal() {
  if (term) {
    term.focus();
  }
}

onMounted(async () => {
  // 1. 等 DOM 渲染
  await nextTick();

  // 2. 等待容器有非零尺寸（解决"display:none 时初始化 → cols=0"问题）
  await waitForContainer();

  // 3. 初始化 xterm
  initPtyTerminal();

  // 4. 第一次 fit
  await nextTick();
  if (fitAddon) {
    fitAddon.fit();
  }

  // 5. 强制 focus（多次尝试）
  setTimeout(tryFocusTerminal, 100);
  setTimeout(tryFocusTerminal, 300);
  setTimeout(tryFocusTerminal, 600);
  setTimeout(tryFocusTerminal, 1000);

  // 6. 设置事件监听 + 加载已有 session
  await setupPtyListeners();
  await loadPtySessions();
  if (ptySessions.value.length === 0) {
    await nextTick();
    await createPtySession();
    // 创建后再 fit + focus
    setTimeout(() => {
      if (fitAddon) fitAddon.fit();
      tryFocusTerminal();
    }, 300);
  }
});

onUnmounted(() => {
  window.removeEventListener("resize", handleTerminalResize);
  resizeObserver?.disconnect();
  resizeObserver = null;
  unlistenPtyOutput?.();
  unlistenPtyExit?.();
  if (term) {
    term.dispose();
    term = null;
  }
  if (fitAddon) {
    fitAddon = null;
  }
});
</script>

<template>
  <NCard :bordered="true" size="small" class="terminal-card">
    <div class="terminal-header" @click="toggleTerminal">
      <div class="terminal-header-left">
        <TerminalIcon :size="16" class="terminal-icon" />
        <span class="terminal-title">终端</span>
        <div class="session-tabs">
          <div
            v-for="s in ptySessions"
            :key="s.session_id"
            class="session-tab"
            :class="{ active: s.session_id === activeSessionId, dead: !s.is_alive }"
            @click.stop="activeSessionId = s.session_id"
          >
            <span class="tab-label">
              {{ s.shell.split('/').pop()?.split('\\').pop() }}
            </span>
            <button class="tab-close" @click.stop="closePtySession(s.session_id)">
              <X :size="12" />
            </button>
          </div>
          <button class="tab-add" @click.stop="createPtySession" title="新建终端">
            <Plus :size="14" />
          </button>
        </div>
      </div>
      <div class="terminal-header-right">
        <span v-if="activeSessionId" class="session-status">
          {{
            ptySessions.find((s) => s.session_id === activeSessionId)?.is_alive
              ? "运行中"
              : "已结束"
          }}
        </span>
        <component
          :is="terminalCollapsed ? ChevronDown : ChevronUp"
          :size="16"
          class="collapse-icon"
        />
      </div>
    </div>
    <div v-show="!terminalCollapsed" class="terminal-container-wrapper">
      <div
        ref="terminalContainer"
        class="terminal-container"
        tabindex="0"
        @click="onTerminalClick"
        @dblclick="onTerminalDoubleClick"
        @focus="onTerminalDoubleClick"
      ></div>
    </div>
  </NCard>
</template>

<style scoped>
.terminal-card {
  display: flex;
  flex-direction: column;
  height: 360px;
  flex-shrink: 0;
  margin-top: 12px;
}

.terminal-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 8px 12px;
  cursor: pointer;
  user-select: none;
  border-bottom: 1px solid rgba(255, 255, 255, 0.05);
}

.terminal-header-left {
  display: flex;
  align-items: center;
  gap: 12px;
  flex: 1;
  min-width: 0;
}

.terminal-icon {
  color: #888;
}

.terminal-title {
  font-weight: 600;
  font-size: 14px;
  white-space: nowrap;
}

.session-tabs {
  display: flex;
  align-items: center;
  gap: 4px;
  overflow-x: auto;
  max-width: 600px;
}

.session-tab {
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 4px 8px;
  border-radius: 4px;
  font-size: 12px;
  background: rgba(255, 255, 255, 0.05);
  color: #ccc;
  cursor: pointer;
  white-space: nowrap;
  border: 1px solid transparent;
  transition: all 0.15s ease;
}

.session-tab:hover {
  background: rgba(255, 255, 255, 0.1);
}

.session-tab.active {
  background: rgba(24, 160, 88, 0.2);
  border-color: rgba(24, 160, 88, 0.5);
  color: #fff;
}

.session-tab.dead {
  opacity: 0.5;
  text-decoration: line-through;
}

.tab-close {
  background: transparent;
  border: none;
  color: inherit;
  cursor: pointer;
  padding: 0;
  display: flex;
  align-items: center;
  opacity: 0.5;
  transition: opacity 0.15s;
}

.tab-close:hover {
  opacity: 1;
}

.tab-add {
  background: transparent;
  border: 1px dashed rgba(255, 255, 255, 0.2);
  color: #888;
  cursor: pointer;
  padding: 2px 6px;
  border-radius: 4px;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: all 0.15s;
}

.tab-add:hover {
  border-color: rgba(24, 160, 88, 0.5);
  color: #18a058;
}

.terminal-header-right {
  display: flex;
  align-items: center;
  gap: 8px;
  color: #888;
  font-size: 12px;
}

.session-status {
  font-family: monospace;
}

.collapse-icon {
  color: #888;
  transition: transform 0.15s;
}

.terminal-container-wrapper {
  flex: 1;
  min-height: 0;
  padding: 0;
  background: #1e1e1e;
}

.terminal-container {
  width: 100%;
  height: 100%;
  min-height: 280px;
  outline: none;
}

.terminal-container:focus,
.terminal-container:focus-within {
  box-shadow: inset 0 0 0 2px rgba(24, 160, 88, 0.5);
}
</style>
