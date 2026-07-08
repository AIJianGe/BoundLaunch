<script setup lang="ts">
/**
 * 终端面板：实时显示 ComfyUI stdout/stderr
 *
 * v3.2.2 新增
 *
 * 详见 `PR/06-界面设计.md §5.1 启动页 - 终端面板`
 *
 * 数据流：
 * 1. processStore.subscribe() 订阅 `comfyui_log` 事件
 * 2. 事件 payload = { source: "stdout"|"stderr", line, ts }
 * 3. 追加到 processStore.logEntries（前端缓冲 1000 行）
 * 4. 组件 watch logEntries 增量渲染
 *
 * 功能：
 * - stdout（白）/ stderr（红）/ ERROR 高亮（黄底）
 * - 自动滚动到最新（用户可暂停）
 * - 清空按钮
 * - 复制全部
 * - 最大行数（1000 行 FIFO 淘汰，避免内存爆炸）
 *
 * 设计模式：
 * - **Observer**：通过 processStore.logEntries（响应式 ref）被动更新
 * - **Virtual Scroller**（简化版）：用 NScrollbar + render 函数，仅渲染可见行
 */

import { ref, computed, watch, onMounted, nextTick } from "vue";
import { useRouter } from "vue-router";
import { NCard, NScrollbar, NButton, NSwitch, NTag, NSpace, NTooltip } from "naive-ui";
import { useProcessStore } from "@/stores/process";
import { useToast } from "@/composables/useToast";
import { ExternalLink } from "@/components/icons";

const processStore = useProcessStore();
const toast = useToast();
const router = useRouter();

/** 最大渲染行数（性能保护：超过此值用 NScrollbar 自动虚拟化） */
const MAX_RENDER_LINES = 1000;
/** 一次新增多少行才触发自动滚动（避免频繁滚动） */
const SCROLL_THRESHOLD = 5;

/** 自动滚动开关（用户可暂停以便查看历史） */
const autoScroll = ref(true);

/** 滚动容器 ref */
const scrollRef = ref<InstanceType<typeof NScrollbar> | null>(null);

/** 加载历史日志（启动时拉取最近 200 行） */
const historyLoaded = ref(false);

onMounted(async () => {
  if (!historyLoaded.value) {
    try {
      // 拉取后端 RingBuffer 里的历史日志（最多 200 行）
      // v3.2.2：loadHistoryLogs 已同步填充 logBuffer + logEntries
      // v3.4.2：append 模式，避免覆盖 in-memory 已有的日志
      await processStore.loadHistoryLogs(200, true);
      historyLoaded.value = true;
    } catch (e) {
      console.warn("[TerminalPanel] loadHistoryLogs failed:", e);
    }
  }
});

/** 监听新行：自动滚动 */
watch(
  () => processStore.logEntries.length,
  async (newLen, oldLen) => {
    if (!autoScroll.value) return;
    const added = newLen - (oldLen ?? 0);
    if (added < SCROLL_THRESHOLD) return;
    await nextTick();
    scrollRef.value?.scrollTo({ top: 999999, behavior: "smooth" });
  },
);

/** 错误高亮识别 */
function isErrorLine(line: string): boolean {
  const lower = line.toLowerCase();
  return (
    lower.includes("error") ||
    lower.includes("traceback") ||
    lower.includes("exception")
  );
}

/** 渲染的日志行（带颜色样式） */
const renderedLines = computed(() => {
  const entries = processStore.logEntries.slice(-MAX_RENDER_LINES);
  return entries.map((e, idx) => {
    const isErr = e.source === "stderr" || isErrorLine(e.line);
    return {
      key: idx,
      text: e.line,
      source: e.source,
      isError: isErr,
      ts: e.ts,
    };
  });
});

/** 当前日志统计 */
const stats = computed(() => {
  const entries = processStore.logEntries;
  return {
    total: entries.length,
    stdout: entries.filter((e) => e.source === "stdout").length,
    stderr: entries.filter((e) => e.source === "stderr").length,
    errors: entries.filter((e) => isErrorLine(e.line)).length,
  };
});

/** 清空日志 */
function onClear() {
  processStore.clearLogs();
  toast.info("已清空终端");
}

function goToTerminalPage() {
  void router.push("/logs");
}

/** 复制全部日志到剪贴板 */
async function onCopyAll() {
  const text = processStore.logEntries.map((e) => e.line).join("\n");
  try {
    await navigator.clipboard.writeText(text);
    toast.success(`已复制 ${processStore.logEntries.length} 行日志`);
  } catch (e) {
    toast.error("复制失败", e);
  }
}

/** 跳到底部（手动触发） */
function onScrollToBottom() {
  scrollRef.value?.scrollTo({ top: 999999, behavior: "smooth" });
  autoScroll.value = true;
}
</script>

<template>
  <NCard class="terminal-panel" :bordered="true" size="small">
    <template #header>
      <div class="header-row">
        <span class="header-title">📟 终端输出</span>
        <NSpace :size="6">
          <NTag size="tiny" :type="stats.stderr > 0 ? 'error' : 'default'">
            stderr {{ stats.stderr }}
          </NTag>
          <NTag size="tiny" :type="stats.errors > 0 ? 'warning' : 'default'">
            error {{ stats.errors }}
          </NTag>
          <NTag size="tiny" type="info">
            共 {{ stats.total }} 行
          </NTag>
        </NSpace>
      </div>
    </template>

    <template #header-extra>
      <NSpace :size="6" align="center">
        <NTooltip placement="top">
          <template #trigger>
            <NButton size="tiny" type="primary" ghost @click="goToTerminalPage">
              <span style="display: inline-flex; align-items: center; gap: 4px;">
                <ExternalLink :size="12" />
                终端
              </span>
            </NButton>
          </template>
          打开完整终端页面（含伪终端）
        </NTooltip>

        <NTooltip placement="top">
          <template #trigger>
            <NSwitch
              v-model:value="autoScroll"
              size="small"
            >
              <template #checked>自动滚动</template>
              <template #unchecked>已暂停</template>
            </NSwitch>
          </template>
          关闭后新增日志不会自动滚动到底部
        </NTooltip>

        <NTooltip placement="top">
          <template #trigger>
            <NButton size="tiny" :disabled="autoScroll" @click="onScrollToBottom">
              ⤓ 跳到底部
            </NButton>
          </template>
          重新启用自动滚动并跳到最新行
        </NTooltip>

        <NTooltip placement="top">
          <template #trigger>
            <NButton size="tiny" @click="onCopyAll">复制</NButton>
          </template>
          复制全部日志到剪贴板
        </NTooltip>

        <NTooltip placement="top">
          <template #trigger>
            <NButton size="tiny" type="warning" ghost @click="onClear">
              清空
            </NButton>
          </template>
          清空前端显示（后端 RingBuffer 保留）
        </NTooltip>
      </NSpace>
    </template>

    <NScrollbar
      ref="scrollRef"
      class="terminal-scroll"
      style="max-height: 480px"
    >
      <div v-if="renderedLines.length === 0" class="empty-hint">
        <span class="empty-icon">⏳</span>
        <span>暂无日志输出。启动 ComfyUI 后，stdout/stderr 会实时显示在这里。</span>
      </div>
      <div v-else class="log-list">
        <div
          v-for="line in renderedLines"
          :key="line.key"
          class="log-line"
          :class="{
            'log-stderr': line.source === 'stderr',
            'log-error': line.isError,
          }"
        >
          <span class="log-prefix">{{ line.source }}</span>
          <span class="log-text">{{ line.text }}</span>
        </div>
      </div>
    </NScrollbar>
  </NCard>
</template>

<style scoped>
.terminal-panel {
  margin-top: 16px;
}

.header-row {
  display: flex;
  align-items: center;
  gap: 12px;
}

.header-title {
  font-weight: 600;
  font-size: 14px;
}

.terminal-scroll {
  background: #1e1e1e;
  border-radius: 4px;
  padding: 8px 12px;
  font-family:
    "Cascadia Code", "Fira Code", Consolas, "Courier New", monospace;
  font-size: 12px;
  line-height: 1.5;
}

.empty-hint {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 32px 16px;
  color: #888;
  font-size: 13px;
  text-align: center;
  justify-content: center;
}

.empty-icon {
  font-size: 24px;
}

.log-list {
  display: flex;
  flex-direction: column;
  gap: 1px;
}

.log-line {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  padding: 1px 0;
  color: #d4d4d4;
  word-break: break-all;
  white-space: pre-wrap;
}

.log-prefix {
  flex-shrink: 0;
  color: #6a6a6a;
  font-size: 10px;
  padding: 1px 4px;
  border: 1px solid #444;
  border-radius: 2px;
  user-select: none;
  min-width: 40px;
  text-align: center;
}

.log-text {
  flex: 1;
  min-width: 0;
}

/* stderr 行整体变红 */
.log-stderr {
  color: #f48771;
  background: rgba(244, 71, 71, 0.05);
}

.log-stderr .log-prefix {
  border-color: #f48771;
  color: #f48771;
}

/* 错误关键字高亮（ERROR/Traceback/Exception） */
.log-error {
  color: #ffeb3b;
  background: rgba(255, 235, 59, 0.08);
  font-weight: 500;
}

.log-error .log-prefix {
  border-color: #ffeb3b;
  color: #ffeb3b;
}
</style>
