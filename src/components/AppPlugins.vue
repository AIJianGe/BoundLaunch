<script setup lang="ts">
/**
 * AppPlugins - 全局插件装载点
 *
 * **必须渲染在所有 Naive UI Provider 内部**（NMessageProvider / NNotificationProvider / NDialogProvider）。
 *
 * 原因：
 * - useCrashRecovery / useTray 内部使用 useToast() → useMessage() + useNotification()
 * - useCrashRecovery 内部使用 useConfirm() → useDialog()
 * - naive-ui@2.38 的这些 hooks 在找不到对应 Provider 时会 throwError
 * - App.vue 的 setup 执行时模板还未渲染，Provider 尚未挂载，inject 返回 null
 * - 因此必须把这些插件调用放到 Provider 的子组件中，子组件 setup 时 Provider 已挂载
 *
 * 设计模式：
 * - **Plugin Holder**：纯装载点，不渲染可见 UI
 *
 * 详见 `PR/06-界面设计.md §7 全局插件`
 */

import { useCrashRecovery } from "@/plugins/crashRecovery";
import { useShortcuts } from "@/plugins/shortcut";
import { useTray } from "@/plugins/tray";

// 在 Provider 内部 setup，inject 可正常获取 message/dialog/notification API
const crashRecovery = useCrashRecovery();
const shortcuts = useShortcuts();
const tray = useTray();

// onMounted 中订阅事件 + 加载初始数据
import { onMounted, onUnmounted } from "vue";
import { useConfigStore } from "@/stores/config";
import { useProcessStore } from "@/stores/process";
import { useEnvStore } from "@/stores/env";
import { useTaskStore } from "@/stores/task";
import { useCoreStore } from "@/stores/core";

const configStore = useConfigStore();
const processStore = useProcessStore();
const envStore = useEnvStore();
const taskStore = useTaskStore();
const coreStore = useCoreStore();

onMounted(async () => {
  // 并行订阅所有事件 + 加载初始数据
  await Promise.all([
    configStore.subscribe(),
    processStore.subscribe(),
    envStore.subscribe(),
    taskStore.subscribe(),
    coreStore.subscribe(),
  ]);

  // 加载初始 Config（其他 store 由事件驱动或页面手动加载）
  try {
    await configStore.load();
  } catch (e) {
    console.error("[App] config load failed:", e);
  }
});

onUnmounted(() => {
  configStore.unsubscribe();
  processStore.unsubscribe();
  envStore.unsubscribe();
  taskStore.unsubscribe();
  coreStore.unsubscribe();
  crashRecovery.cleanup();
  shortcuts.cleanup();
  tray.cleanup();
});
</script>

<template>
  <!-- 纯装载点，不渲染可见 UI，仅透传 slot（RouterView） -->
  <slot />
</template>
