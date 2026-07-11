<script setup lang="ts">
/**
 * 设置页（容器）
 *
 * 详见 `PR/06-界面设计.md §5.3 设置页`
 *
 * 6 区编排：
 * 1. 路径配置         PathsPanel
 * 2. Python 切换      PythonVersionPanel
 * 3. torch 配置       TorchConfigPanel
 * 4. transformers 切换 TransformersConfigPanel（v3.7 新增）
 * 5. UI 配置           UiPanel
 * 6. 危险操作          DangerZonePanel
 *
 * 容器职责：
 * - 加载初始 Config / envInfo（如未加载）
 * - 编排 6 个子组件
 *
 * 设计模式：
 * - **Facade**：本页面是「设置页」外观
 * - **Repository**：通过 store 访问后端
 */

import { onMounted } from "vue";
import { NSpin } from "naive-ui";
import { useConfigStore } from "@/stores/config";
import { useEnvStore } from "@/stores/env";
import { computed } from "vue";
import PathsPanel from "@/components/settings/PathsPanel.vue";
import PythonVersionPanel from "@/components/settings/PythonVersionPanel.vue";
import TorchConfigPanel from "@/components/settings/TorchConfigPanel.vue";
import TransformersConfigPanel from "@/components/settings/TransformersConfigPanel.vue";
import DependenciesPanel from "@/components/settings/DependenciesPanel.vue";
import UiPanel from "@/components/settings/UiPanel.vue";
import DangerZonePanel from "@/components/settings/DangerZonePanel.vue";
import DataLocationPanel from "@/components/settings/DataLocationPanel.vue"; // v1.8 / F38
import GpuPanel from "@/components/settings/GpuPanel.vue"; // v3.x Phase 5
import LaunchAdvancedPanel from "@/components/settings/LaunchAdvancedPanel.vue"; // v3.x 启动高级参数

const configStore = useConfigStore();
const envStore = useEnvStore();

const initialLoading = computed(
  () => !configStore.isLoaded && !envStore.isLoaded,
);

onMounted(async () => {
  const tasks: Promise<unknown>[] = [];

  if (!configStore.isLoaded) {
    tasks.push(configStore.load().catch((e) => console.warn("config load:", e)));
  }

  if (!envStore.isLoaded) {
    tasks.push(envStore.refresh().catch((e) => console.warn("env refresh:", e)));
  }

  await Promise.allSettled(tasks);
});
</script>

<template>
  <div class="settings-page">
    <div v-if="initialLoading" class="page-loading">
      <NSpin size="medium" />
      <span class="loading-text">加载设置页...</span>
    </div>

    <template v-else>
      <!-- v1.8 / F38：数据位置信息（顶部优先显示） -->
      <DataLocationPanel />
      <PathsPanel />
      <PythonVersionPanel />
      <TorchConfigPanel />
      <GpuPanel />
      <!-- v3.x：启动高级参数（"使用共享显存"开关）放在 GPU 选择后 -->
      <LaunchAdvancedPanel />
      <TransformersConfigPanel />
      <DependenciesPanel />
      <UiPanel />
      <DangerZonePanel />
    </template>
  </div>
</template>

<style scoped>
.settings-page {
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
