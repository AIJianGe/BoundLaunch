<script setup lang="ts">
/**
 * §3.8 启动后状态区
 *
 * 详见 `PR/06-界面设计.md §3.8 启动后状态区`
 *
 * 三态展示：
 * - 未运行：○ 灰色圆点 + 服务地址占位
 * - 启动中：◔ 黄色加载圈 + 等待健康检查
 * - 运行中：● 绿色圆点 + 服务地址 + [打开浏览器]
 *
 * 行为：
 * - 点击「打开浏览器」调用 window.open() 打开 http://{host}:{port}
 * - PID / 启动时间 / 运行时长 仅运行中显示
 *
 * 数据来源：processStore.status（discriminated union）
 */

import { computed } from "vue";
import { NCard, NButton, NSpin, NTag, NSpace } from "naive-ui";
import { useProcessStore } from "@/stores/process";
import { useConfigStore } from "@/stores/config";
import { useToast } from "@/composables/useToast";

const processStore = useProcessStore();
const configStore = useConfigStore();
const toast = useToast();

const listenHost = computed(() => configStore.config?.launch.listen_host ?? "127.0.0.1");
const listenPort = computed(() => configStore.config?.launch.listen_port ?? 8188);

const serviceUrl = computed(() => {
  // 0.0.0.0 在浏览器中替换为 127.0.0.1
  const host = listenHost.value === "0.0.0.0" ? "127.0.0.1" : listenHost.value;
  return `http://${host}:${listenPort.value}`;
});

const panelState = computed<"stopped" | "starting" | "running" | "other">(() => {
  switch (processStore.status.kind) {
    case "stopped":
      return "stopped";
    case "starting":
      return "starting";
    case "running":
      return "running";
    default:
      return "other";
  }
});

const startedAt = computed<string | null>(() => {
  if (processStore.status.kind === "running") {
    return processStore.status.started_at;
  }
  return null;
});

const pid = computed<number | null>(() => processStore.pid);

function openBrowser() {
  try {
    window.open(serviceUrl.value, "_blank", "noopener,noreferrer");
  } catch (e) {
    toast.error("打开浏览器失败", e);
  }
}
</script>

<template>
  <NCard class="running-status" :bordered="true" size="small">
    <template #header>
      <span class="header-title">🚦 服务状态</span>
    </template>

    <div class="status-row">
      <!-- 状态指示灯 -->
      <div class="indicator" :class="`indicator-${panelState}`">
        <NSpin v-if="panelState === 'starting'" size="small" />
        <span v-else class="indicator-dot">●</span>
        <span class="indicator-text">
          <template v-if="panelState === 'running'">运行中</template>
          <template v-else-if="panelState === 'starting'">启动中（等待健康检查）</template>
          <template v-else-if="panelState === 'stopped'">未运行</template>
          <template v-else>状态异常</template>
        </span>
      </div>

      <!-- 服务地址 -->
      <NSpace size="small" align="center" class="url-row">
        <span class="label">服务地址:</span>
        <code v-if="panelState === 'running' || panelState === 'starting'" class="url">
          {{ serviceUrl }}
        </code>
        <span v-else class="url-empty">-</span>
      </NSpace>

      <!-- 操作按钮 -->
      <NButton
        v-if="panelState === 'running'"
        size="small"
        type="primary"
        @click="openBrowser"
      >
        打开浏览器
      </NButton>
    </div>

    <!-- 运行中详细信息 -->
    <div v-if="panelState === 'running'" class="running-detail">
      <NSpace size="small" align="center">
        <span class="detail-label">PID:</span>
        <NTag size="small" type="info">{{ pid }}</NTag>
      </NSpace>
      <NSpace v-if="startedAt" size="small" align="center">
        <span class="detail-label">启动时间:</span>
        <span class="detail-value">{{ startedAt }}</span>
      </NSpace>
    </div>

    <div v-if="panelState === 'starting'" class="starting-hint">
      健康检查每 1 秒探测一次，最长等待 60 秒。请耐心等待 ComfyUI 模型加载完成。
    </div>

    <div v-if="panelState === 'stopped'" class="stopped-hint">
      ComfyUI 未运行，点击上方「启动」按钮开始。
    </div>
  </NCard>
</template>

<style scoped>
.running-status {
  margin-bottom: 16px;
}

.header-title {
  font-weight: 600;
}

.status-row {
  display: flex;
  align-items: center;
  gap: 16px;
  flex-wrap: wrap;
}

.indicator {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 14px;
  font-weight: 500;
}

.indicator-dot {
  font-size: 16px;
  line-height: 1;
}

.indicator-running .indicator-dot {
  color: var(--app-success, #18a058);
}

.indicator-starting {
  color: var(--app-warning, #f0a020);
}

.indicator-stopped .indicator-dot {
  color: var(--app-text-muted, #999);
}

.indicator-other .indicator-dot {
  color: var(--app-error, #d03050);
}

.url-row {
  flex: 1;
  min-width: 200px;
}

.label {
  font-size: 13px;
  color: var(--app-text-muted, #999);
}

.url {
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  font-size: 13px;
  color: var(--app-primary, #2080f0);
}

.url-empty {
  color: var(--app-text-muted, #999);
}

.running-detail {
  display: flex;
  gap: 24px;
  margin-top: 12px;
  padding-top: 12px;
  border-top: 1px solid var(--app-border, rgba(127, 127, 127, 0.1));
  font-size: 12px;
}

.detail-label {
  color: var(--app-text-muted, #999);
}

.detail-value {
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
}

.starting-hint,
.stopped-hint {
  margin-top: 12px;
  padding: 8px 12px;
  background: var(--app-bg-soft, rgba(127, 127, 127, 0.05));
  border-radius: 4px;
  font-size: 12px;
  color: var(--app-text-muted, #999);
}
</style>
