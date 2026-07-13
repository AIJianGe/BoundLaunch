<script setup lang="ts">
/**
 * ErrorPanelCard.vue — 业务错误面板（v3.13 移入"安装日志"tab 内部）
 *
 * 设计目的：
 * - 把所有 useToast.error / warn 调用的错误以"面板"形式集中显示
 * - 永远不消失（用户可主动清空显示）
 * - v3.13：从 LogsPage 顶部移到 InstallLogsTab 内部，节省首页空间
 *   - 菜单红点未读数仍然有效（进入 LogsPage 即"已读"）
 *   - 用户切到"安装日志" tab 即可看到面板
 *
 * 数据源：
 * - useErrorLog composable（pinia store）
 *   - recentErrors：最近 50 条（应用启动后累积）
 *   - 面板内显示最近 20 条（tab 内部空间更大）
 *   - 订阅 business_log 事件自动更新
 *
 * 与 install-logs 的区别：
 * - ErrorPanel：只显示 error/warn 级别，置顶、突出、按时间倒序
 * - InstallLogs：所有级别、按阶段过滤、可滚动历史
 *
 * 红点清除：
 * - 进入 LogsPage 时 useErrorLog.markAllRead() 会清零菜单红点
 */

import {
  NCard,
  NSpace,
  NTag,
  NButton,
  NPopconfirm,
  NText,
} from "naive-ui";
import { useErrorLog } from "@/composables/useErrorLog";

const errorLog = useErrorLog();

/** v3.13：tab 内部空间更大，扩大到 20 条（displayErrors 仍保留 10 条给其他场景用） */
const PANEL_DISPLAY_LIMIT = 20;

function formatErrorTime(ts: string): string {
  return ts.split("T")[1]?.split(".")[0] || ts;
}

async function onRefreshErrors() {
  await errorLog.loadHistory();
}
</script>

<template>
  <NCard
    v-if="errorLog.hasErrors"
    class="error-panel"
    :bordered="true"
    size="small"
  >
    <template #header>
      <div class="error-panel-header">
        <span class="error-panel-title">
          ⚠ 最近错误（{{ errorLog.recentErrors.length }}）
        </span>
      </div>
    </template>
    <NSpace vertical size="small">
      <div
        v-for="(err, idx) in errorLog.recentErrors.slice(0, PANEL_DISPLAY_LIMIT)"
        :key="err.ts + idx"
        class="error-item"
      >
        <div class="error-item-header">
          <NTag size="small" :type="err.level === 'error' ? 'error' : 'warning'">
            {{ err.level.toUpperCase() }}
          </NTag>
          <span class="error-item-time">{{ formatErrorTime(err.ts) }}</span>
          <span class="error-item-source">[{{ err.source }}]</span>
        </div>
        <div class="error-item-message">{{ err.message }}</div>
        <details v-if="err.detail" class="error-item-detail">
          <summary>展开详情</summary>
          <pre>{{ err.detail }}</pre>
        </details>
      </div>
      <div
        v-if="errorLog.recentErrors.length > PANEL_DISPLAY_LIMIT"
        class="error-panel-hint"
      >
        仅显示前 {{ PANEL_DISPLAY_LIMIT }} 条，完整历史见下方日志流（已持久化到 LogStore）
      </div>
      <NSpace size="small">
        <NButton size="tiny" @click="onRefreshErrors">刷新历史</NButton>
        <NPopconfirm
          :on-positive-click="errorLog.clearDisplayed"
          positive-text="确认清空"
          negative-text="取消"
        >
          <template #trigger>
            <NButton size="tiny" type="warning" ghost>清空显示</NButton>
          </template>
          仅清空面板显示，LogStore 数据不动
        </NPopconfirm>
      </NSpace>
    </NSpace>
  </NCard>
</template>

<style scoped>
.error-panel {
  flex-shrink: 0;
  /* v3.13：现在在 tab 内部，底部留 12px 与下方工具栏隔开 */
  margin-bottom: 12px;
  border-color: #d03050;
  background: linear-gradient(135deg, #fef0f0 0%, #ffffff 100%);
}

.error-panel-header {
  display: flex;
  align-items: center;
  gap: 8px;
}

.error-panel-title {
  font-weight: 600;
  color: #d03050;
}

.error-item {
  padding: 8px 12px;
  border-left: 3px solid #d03050;
  background: #fafafa;
  border-radius: 4px;
}

.error-item-header {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  color: #888;
}

.error-item-time {
  font-family: monospace;
}

.error-item-source {
  font-family: monospace;
  color: #555;
}

.error-item-message {
  margin-top: 4px;
  font-size: 14px;
  color: #333;
}

.error-item-detail {
  margin-top: 4px;
  font-size: 12px;
}

.error-item-detail summary {
  cursor: pointer;
  color: #888;
  user-select: none;
}

.error-item-detail pre {
  margin-top: 4px;
  padding: 8px;
  background: #fff;
  border: 1px solid #e0e0e0;
  border-radius: 4px;
  white-space: pre-wrap;
  word-break: break-all;
  font-size: 12px;
}

.error-panel-hint {
  font-size: 12px;
  color: #888;
  font-style: italic;
}
</style>
