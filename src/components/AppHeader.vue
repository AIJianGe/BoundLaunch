<script setup lang="ts">
/**
 * AppHeader - 顶栏组件
 *
 * 详见 `PR/06-界面设计.md §2 顶栏与全局状态`
 *
 * 内容：
 * - 左侧：项目名 myComfyUI + 火箭图标
 * - 中间：版本号
 * - 右侧：ComfyUI 进程状态指示（4 态） + 设置入口
 *
 * 状态指示 4 态：
 * - 未运行（灰）
 * - 启动中（黄 loading）
 * - 运行中（绿，含端口）
 * - 环境异常（红，点击跳设置页）
 */

import { computed } from "vue";
import { useRouter } from "vue-router";
import { NButton, NTooltip, NTag, NSpin } from "naive-ui";
import { useProcessStore } from "@/stores/process";
import { useEnvStore } from "@/stores/env";

const router = useRouter();
const processStore = useProcessStore();
const envStore = useEnvStore();

/** launcher 版本号（由 vite.config.ts 注入为全局常量） */
const launcherVersion = __APP_VERSION__;

/** 状态指示 4 态 */
type StatusKind = "stopped" | "starting" | "running" | "env_error";

const statusKind = computed<StatusKind>(() => {
  // 环境异常优先（torch 缺失 / venv 不存在）
  if (envStore.isLoaded && (!envStore.venvExists || !envStore.torchInstalled)) {
    return "env_error";
  }
  if (processStore.isStarting) return "starting";
  if (processStore.isRunning) return "running";
  return "stopped";
});

const statusConfig = computed(() => {
  switch (statusKind.value) {
    case "running":
      return {
        color: "success" as const,
        label: `运行中 :${processStore.port ?? 8188}`,
        tooltip: `PID: ${processStore.pid ?? "-"}，点击打开浏览器`,
      };
    case "starting":
      return {
        color: "warning" as const,
        label: "启动中...",
        tooltip: "正在等待健康检查通过",
      };
    case "env_error":
      return {
        color: "error" as const,
        label: "环境异常",
        tooltip: "点击跳转设置页修复环境",
      };
    default:
      return {
        color: "default" as const,
        label: "未运行",
        tooltip: "点击聚焦启动页",
      };
  }
});

function onStatusClick() {
  if (statusKind.value === "env_error") {
    router.push("/settings");
  } else if (statusKind.value === "running") {
    // 打开浏览器（ComfyUI Web UI）
    const port = processStore.port ?? 8188;
    window.open(`http://127.0.0.1:${port}`, "_blank");
  } else {
    router.push("/launch");
  }
}

function goSettings() {
  router.push("/settings");
}
</script>

<template>
  <div class="app-header">
    <div class="header-left">
      <span class="rocket-icon">🚀</span>
      <span class="app-name">myComfyUI</span>
      <span class="app-version">v{{ launcherVersion }}</span>
    </div>
    <div class="header-right">
      <NTooltip placement="bottom">
        <template #trigger>
          <NTag
            :type="statusConfig.color"
            :bordered="false"
            size="small"
            round
            class="status-tag"
            :class="{ clickable: true }"
            @click="onStatusClick"
          >
            <template #icon>
              <NSpin v-if="statusKind === 'starting'" size="small" />
              <span v-else>●</span>
            </template>
            {{ statusConfig.label }}
          </NTag>
        </template>
        {{ statusConfig.tooltip }}
      </NTooltip>

      <NTooltip placement="bottom">
        <template #trigger>
          <NButton quaternary circle @click="goSettings">
            ⚙️
          </NButton>
        </template>
        设置
      </NTooltip>
    </div>
  </div>
</template>

<style scoped>
.app-header {
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 16px;
}

.header-left {
  display: flex;
  align-items: center;
  gap: 8px;
}

.rocket-icon {
  font-size: 18px;
}

.app-name {
  font-weight: 600;
  font-size: 16px;
}

.app-version {
  font-size: 12px;
  opacity: 0.6;
  margin-left: 4px;
}

.header-right {
  display: flex;
  align-items: center;
  gap: 8px;
}

.status-tag {
  cursor: pointer;
  user-select: none;
}

.status-tag:hover {
  filter: brightness(1.05);
}
</style>
