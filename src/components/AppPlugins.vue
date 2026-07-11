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
import { onMounted, onUnmounted, ref } from "vue";
import { useConfigStore } from "@/stores/config";
import { useProcessStore } from "@/stores/process";
import { useEnvStore } from "@/stores/env";
import { useTaskStore } from "@/stores/task";
import { useCoreStore } from "@/stores/core";
import { usePluginStore } from "@/stores/plugin";
import { useErrorLog } from "@/composables/useErrorLog";

// v3.x Phase 3/2：全局弹窗引用
import HardwareChangeDialog from "@/components/system/HardwareChangeDialog.vue";
import TorchInstallFallbackDialog from "@/components/system/TorchInstallFallbackDialog.vue";

const configStore = useConfigStore();
const processStore = useProcessStore();
const envStore = useEnvStore();
const taskStore = useTaskStore();
const coreStore = useCoreStore();
const pluginStore = usePluginStore();

// v3.10：全局错误日志 store（ErrorPanel + 菜单红点）
const errorLog = useErrorLog();

const hardwareDialog = ref<InstanceType<typeof HardwareChangeDialog> | null>(null);
const torchDialog = ref<InstanceType<typeof TorchInstallFallbackDialog> | null>(null);

/**
 * v3.x Phase 3：启动后延迟 7s 检测硬件变化
 *
 * 延迟原因：
 * - 主流程 / UI 渲染需要先完成
 * - GPU 探测可能慢（nvidia-smi 首次调用需 1-2s）
 * - 不要阻塞启动
 */
function scheduleHardwareCheck() {
  setTimeout(() => {
    hardwareDialog.value?.check();
  }, 7000);
}

/**
 * v3.x Phase 2：监听 torch 安装失败事件
 */
function onTorchIncompatible(e: Event) {
  const custom = e as CustomEvent<string>;
  if (custom.detail) {
    torchDialog.value?.show(custom.detail);
  }
}

onMounted(async () => {
  // 并行订阅所有事件 + 加载初始数据
  await Promise.all([
    configStore.subscribe(),
    processStore.subscribe(),
    envStore.subscribe(),
    taskStore.subscribe(),
    coreStore.subscribe(),
    // v3.x：订阅插件列表变更事件（install/uninstall/toggle 后 emit）
    pluginStore.subscribe(),
    // v3.10：订阅后端 business_log 事件 + 拉取历史错误
    errorLog.subscribe(),
    errorLog.loadHistory(),
  ]);

  // 加载初始 Config（其他 store 由事件驱动或页面手动加载）
  try {
    await configStore.load();
  } catch (e) {
    console.error("[App] config load failed:", e);
  }

  // v3.x Phase 3：硬件变化检测（启动后 7s 触发）
  scheduleHardwareCheck();

  // v3.x Phase 2：监听 torch 安装失败
  window.addEventListener("torch-install-incompatible", onTorchIncompatible);
});

onUnmounted(() => {
  configStore.unsubscribe();
  processStore.unsubscribe();
  envStore.unsubscribe();
  taskStore.unsubscribe();
  coreStore.unsubscribe();
  // v3.x：取消插件事件订阅
  pluginStore.unsubscribe();
  // v3.10：取消 business_log 订阅
  errorLog.unsubscribe();
  window.removeEventListener("torch-install-incompatible", onTorchIncompatible);
  crashRecovery.cleanup();
  shortcuts.cleanup();
  tray.cleanup();
});
</script>

<template>
  <!-- 全局弹窗（不影响 layout） -->
  <HardwareChangeDialog ref="hardwareDialog" />
  <TorchInstallFallbackDialog ref="torchDialog" />
  <!-- 纯装载点，不渲染可见 UI，仅透传 slot（RouterView） -->
  <slot />
</template>
