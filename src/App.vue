<script setup lang="ts">
/**
 * App - 根组件
 *
 * 职责：
 * - 提供 Naive UI 全局 Provider（ConfigProvider / LoadingBarProvider / MessageProvider / NotificationProvider / DialogProvider）
 * - 提供 主题切换（NConfigProvider :theme）
 *
 * 注意：
 * - 使用 useMessage / useNotification / useDialog 的插件（useCrashRecovery / useTray 等）
 *   必须在 Provider 内部的子组件中调用，不能在 App.vue 的 setup 中调用
 *   （naive-ui@2.38 在找不到 Provider 时会 throwError，导致应用挂载失败 → 白屏）
 * - 这些插件已移到 AppPlugins.vue，作为 Provider 内部的子组件渲染
 *
 * 详见 `PR/06-界面设计.md §0 应用根组件`
 */

import {
  NConfigProvider,
  NMessageProvider,
  NNotificationProvider,
  NDialogProvider,
  NLoadingBarProvider,
  zhCN,
  dateZhCN,
  darkTheme,
} from "naive-ui";
import { computed } from "vue";
import { useThemeStore } from "@/stores/theme";
import AppPlugins from "@/components/AppPlugins.vue";

const themeStore = useThemeStore();
const theme = computed(() => (themeStore.isDark ? darkTheme : null));
</script>

<template>
  <NConfigProvider :theme="theme" :locale="zhCN" :date-locale="dateZhCN">
    <NLoadingBarProvider>
      <NMessageProvider>
        <NNotificationProvider>
          <NDialogProvider>
            <!-- AppPlugins 必须在所有 Provider 内部，其 setup 中调用 useToast/useConfirm -->
            <AppPlugins>
              <RouterView />
            </AppPlugins>
          </NDialogProvider>
        </NNotificationProvider>
      </NMessageProvider>
    </NLoadingBarProvider>
  </NConfigProvider>
</template>
