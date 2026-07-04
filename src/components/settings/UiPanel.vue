<script setup lang="ts">
/**
 * UI 配置面板
 *
 * 详见 `PR/06-界面设计.md §5.3 设置页 - UI 配置`
 *
 * 字段：
 * - theme: light / dark / auto（跟随系统）
 * - language: zh-CN（本期仅 zh-CN，预留 en-US）
 * - auto_check_updates（启动时自动检查更新，本期 UI 预留字段，默认开启）
 * - minimize_to_tray（关闭窗口时最小化到托盘，本期预留）
 *
 * 行为：
 * - theme 切换立即生效（themeStore 修改 → App.vue 响应）
 * - 其他字段调用 configStore.update
 */

import { computed, watch, ref } from "vue";
import {
  NCard,
  NForm,
  NFormItem,
  NRadioGroup,
  NRadio,
  NSelect,
  NSwitch,
  NSpace,
} from "naive-ui";
import { useConfigStore } from "@/stores/config";
import { useThemeStore } from "@/stores/theme";
import { useToast } from "@/composables/useToast";
import type { Theme } from "@/api/types";

const configStore = useConfigStore();
const themeStore = useThemeStore();
const toast = useToast();

const languageOptions = [
  { label: "简体中文 (zh-CN)", value: "zh-CN" },
  { label: "English (en-US) - 预留", value: "en-US", disabled: true },
];

const currentTheme = ref<Theme>("auto");
const currentLanguage = ref("zh-CN");
const autoCheckUpdates = ref(true);
const minimizeToTray = ref(true);

watch(
  () => configStore.config,
  (cfg) => {
    if (cfg) {
      currentTheme.value = cfg.ui.theme;
      currentLanguage.value = cfg.ui.language;
      // auto_check_updates / minimize_to_tray 字段在 UiConfig 中暂未实现，使用默认值
    }
  },
  { immediate: true },
);

async function onThemeChange(value: Theme) {
  currentTheme.value = value;
  // 立即更新 themeStore（前端立即响应）
  themeStore.setMode(value);
  try {
    await configStore.update({ ui: { theme: value } });
  } catch (e) {
    toast.error("保存失败", e);
  }
}

async function onLanguageChange(value: string) {
  currentLanguage.value = value;
  try {
    await configStore.update({ ui: { language: value } });
  } catch (e) {
    toast.error("保存失败", e);
  }
}

// 以下两个开关本期仅 UI 预留，实际行为未实现
async function onAutoCheckUpdates(value: boolean) {
  autoCheckUpdates.value = value;
  // TODO: 持久化到 UiConfig（需后端扩展字段）
}

async function onMinimizeToTray(value: boolean) {
  minimizeToTray.value = value;
  // TODO: 持久化 + 接入 Tauri 窗口关闭事件
}
</script>

<template>
  <NCard class="ui-panel" :bordered="true" size="small">
    <template #header>
      <span class="header-title">🎨 UI 配置</span>
    </template>

    <NForm label-placement="top" :show-feedback="false" size="small">
      <NFormItem label="主题">
        <NRadioGroup :value="currentTheme" @update:value="onThemeChange">
          <NSpace>
            <NRadio value="light">亮色</NRadio>
            <NRadio value="dark">暗色</NRadio>
            <NRadio value="auto">跟随系统</NRadio>
          </NSpace>
        </NRadioGroup>
      </NFormItem>

      <NFormItem label="语言">
        <NSelect
          :value="currentLanguage"
          :options="languageOptions"
          @update:value="onLanguageChange"
        />
      </NFormItem>

      <NFormItem label="启动时自动检查更新">
        <NSwitch :value="autoCheckUpdates" @update:value="onAutoCheckUpdates" />
        <span class="form-hint">（预留功能，本期未实现）</span>
      </NFormItem>

      <NFormItem label="关闭窗口时最小化到托盘">
        <NSwitch :value="minimizeToTray" @update:value="onMinimizeToTray" />
        <span class="form-hint">（预留功能，保活 ComfyUI）</span>
      </NFormItem>
    </NForm>
  </NCard>
</template>

<style scoped>
.ui-panel {
  margin-bottom: 16px;
}

.header-title {
  font-weight: 600;
}

.form-hint {
  margin-left: 12px;
  font-size: 12px;
  color: var(--app-text-muted, #999);
}
</style>
