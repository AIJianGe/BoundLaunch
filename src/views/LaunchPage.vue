<script setup lang="ts">
/**
 * 启动页（首页容器）
 *
 * 详见 `PR/06-界面设计.md §3 启动页（首页）`
 *
 * 9 区块编排：
 * 1. §3.1 顶部状态卡片      StatusCard
 * 2. §3.2 启动/停止按钮       StartStopButtons
 * 3. §3.3 运行模式单选        LaunchModeSelector
 * 4. §3.4 基础参数表单        BasicParamsForm
 * 5. §3.5 关键依赖列表        DependencyList
 * 6. §3.6 命令预览            CommandPreview
 * 7. §3.7 高级参数折叠        AdvancedParamsPanel
 * 8. §3.8 启动后状态          RunningStatusPanel
 * 9. （可选）§3.9 实时日志面板 - 复用 LogsPage 简化版（本期略，日志页有完整版）
 *
 * 容器职责：
 * - 加载初始 Config / envInfo / 历史日志
 * - 编排子组件
 * - 不直接处理业务逻辑（交由子组件 + store）
 *
 * 设计模式：
 * - **Facade**：本页面是「启动页」外观，对内编排 8 个子组件
 * - **Repository**：通过 store 访问后端，不直接调用 API
 */

import { computed, onMounted, onUnmounted } from "vue";
import { NSpin } from "naive-ui";
import { useConfigStore } from "@/stores/config";
import { useEnvStore } from "@/stores/env";
import { useProcessStore } from "@/stores/process";
import StatusCard from "@/components/launch/StatusCard.vue";
import StartStopButtons from "@/components/launch/StartStopButtons.vue";
import LaunchModeSelector from "@/components/launch/LaunchModeSelector.vue";
import BasicParamsForm from "@/components/launch/BasicParamsForm.vue";
import DependencyList from "@/components/launch/DependencyList.vue";
import CommandPreview from "@/components/launch/CommandPreview.vue";
import AdvancedParamsPanel from "@/components/launch/AdvancedParamsPanel.vue";
import RunningStatusPanel from "@/components/launch/RunningStatusPanel.vue";

const configStore = useConfigStore();
const envStore = useEnvStore();
const processStore = useProcessStore();

onMounted(async () => {
  // 并行加载初始数据（subscribe 已在 App.vue 调用，此处仅补数据）
  const tasks: Promise<unknown>[] = [];

  if (!configStore.isLoaded) {
    tasks.push(configStore.load().catch((e) => console.warn("config load:", e)));
  }

  if (!envStore.isLoaded) {
    tasks.push(envStore.refresh().catch((e) => console.warn("env refresh:", e)));
  }

  tasks.push(processStore.loadHistoryLogs(200).catch((e) => console.warn("history logs:", e)));

  await Promise.allSettled(tasks);
});

onUnmounted(() => {
  // 切换页面时清空日志缓冲（避免内存占用；下次进入会重新拉取）
  // 注意：subscribe 在 App.vue 全局订阅，此处不 unsubscribe
  // processStore.clearLogs(); // 暂不清空，便于用户切换页面回来还能看到
});

/** 初始加载状态（config 与 env 都未加载时显示 loading） */
const initialLoading = computed(
  () => !configStore.isLoaded && !envStore.isLoaded,
);
</script>

<template>
  <div class="launch-page">
    <div v-if="initialLoading" class="page-loading">
      <NSpin size="medium" />
      <span class="loading-text">加载启动页...</span>
    </div>

    <template v-else>
      <!-- §3.1 顶部状态卡片 -->
      <StatusCard />

      <!-- §3.2 启动 / 停止按钮 -->
      <StartStopButtons />

      <!-- §3.3 运行模式单选 -->
      <LaunchModeSelector />

      <!-- §3.4 基础参数表单 -->
      <BasicParamsForm />

      <!-- §3.5 关键依赖列表 -->
      <DependencyList />

      <!-- §3.6 命令预览 -->
      <CommandPreview />

      <!-- §3.7 高级参数折叠 -->
      <AdvancedParamsPanel />

      <!-- §3.8 启动后状态 -->
      <RunningStatusPanel />
    </template>
  </div>
</template>

<style scoped>
.launch-page {
  padding: 16px;
  max-width: 1200px;
  margin: 0 auto;
}

.page-loading {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 12px;
  padding: 80px 0;
}

.loading-text {
  font-size: 13px;
  color: var(--app-text-muted, #999);
}
</style>
