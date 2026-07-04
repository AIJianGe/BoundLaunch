<script setup lang="ts">
/**
 * §3.4 基础参数表单
 *
 * 详见 `PR/06-界面设计.md §3.4 基础参数区`
 *
 * 数据来源：`PR/05-依赖与启动参数.md §3.1`
 *
 * 字段：
 * - 监听地址 listen_host：127.0.0.1 / 0.0.0.0 / 自定义
 * - 端口 listen_port：1-65535，默认 8188
 * - 预览方式 preview_method：latent / latent-upscale / autoencoder / none
 * - 自动打开浏览器 auto_open_browser：开关
 *
 * 行为：
 * - 表单字段绑定到 configStore.config.launch
 * - onChange 时调用 configStore.update（部分更新）
 * - 监听 0.0.0.0 时显示警告「局域网可访问」
 */

import { computed, ref, watch } from "vue";
import {
  NCard,
  NForm,
  NFormItem,
  NInput,
  NInputNumber,
  NSelect,
  NSwitch,
  NAlert,
  NSpace,
} from "naive-ui";
import { useConfigStore } from "@/stores/config";
import { useToast } from "@/composables/useToast";
import type { PreviewMethod } from "@/api/types";

const configStore = useConfigStore();
const toast = useToast();

const launch = computed(() => configStore.config?.launch);

// 本地缓存以避免每次输入都触发后端 update
const localHost = ref("");
const localPort = ref(8188);
const localCustomArgs = ref("");

watch(
  () => configStore.config,
  (cfg) => {
    if (cfg) {
      localHost.value = cfg.launch.listen_host;
      localPort.value = cfg.launch.listen_port;
      localCustomArgs.value = cfg.launch.custom_args;
    }
  },
  { immediate: true },
);

const hostOptions = [
  { label: "127.0.0.1（仅本机，推荐）", value: "127.0.0.1" },
  { label: "0.0.0.0（局域网可访问）", value: "0.0.0.0" },
];

const previewMethodOptions: Array<{ label: string; value: PreviewMethod }> = [
  { label: "latent（默认，快）", value: "latent" },
  { label: "latent-upscale（高质量预览）", value: "latent_upscale" },
  { label: "autoencoder（自动编码器）", value: "autoencoder" },
  { label: "none（不预览）", value: "none" },
];

const isLan = computed(() => localHost.value === "0.0.0.0");

async function updateHost() {
  try {
    await configStore.update({
      launch: { listen_host: localHost.value },
    });
  } catch (e) {
    toast.error("保存失败", e);
  }
}

async function updatePort() {
  if (localPort.value < 1 || localPort.value > 65535) {
    toast.error("端口范围无效", new Error("端口需在 1-65535 之间"));
    return;
  }
  try {
    await configStore.update({
      launch: { listen_port: localPort.value },
    });
  } catch (e) {
    toast.error("保存失败", e);
  }
}

async function updatePreviewMethod(value: PreviewMethod) {
  try {
    await configStore.update({
      launch: { preview_method: value },
    });
  } catch (e) {
    toast.error("保存失败", e);
  }
}

async function updateAutoOpen(value: boolean) {
  try {
    await configStore.update({
      launch: { auto_open_browser: value },
    });
  } catch (e) {
    toast.error("保存失败", e);
  }
}

async function updateCustomArgs() {
  try {
    await configStore.update({
      launch: { custom_args: localCustomArgs.value },
    });
  } catch (e) {
    toast.error("保存失败", e);
  }
}
</script>

<template>
  <NCard class="basic-params" :bordered="true" size="small">
    <template #header>
      <span class="header-title">🔧 基础参数</span>
    </template>

    <div v-if="!launch" class="empty-state">
      <span class="hint">配置未加载</span>
    </div>

    <NForm v-else label-placement="top" :show-feedback="false" size="small">
      <div class="form-grid">
        <NFormItem label="监听地址">
          <NSelect
            v-model:value="localHost"
            :options="hostOptions"
            filterable
            tag
            @update:value="updateHost"
          />
        </NFormItem>

        <NFormItem label="端口">
          <NInputNumber
            v-model:value="localPort"
            :min="1"
            :max="65535"
            :show-button="false"
            @blur="updatePort"
            @update:value="updatePort"
          />
        </NFormItem>

        <NFormItem label="预览方式">
          <NSelect
            :value="launch.preview_method"
            :options="previewMethodOptions"
            @update:value="updatePreviewMethod"
          />
        </NFormItem>

        <NFormItem label="自动打开浏览器">
          <NSwitch
            :value="launch.auto_open_browser"
            @update:value="updateAutoOpen"
          />
        </NFormItem>
      </div>

      <NAlert
        v-if="isLan"
        type="warning"
        :bordered="false"
        class="lan-warn"
      >
        ⚠ 监听 0.0.0.0 允许局域网内其他设备访问本机 ComfyUI，
        请确保网络环境可信；生产环境建议绑定 127.0.0.1。
      </NAlert>

      <NFormItem
        v-if="launch.mode === 'custom'"
        label="自定义参数（custom_args）"
        class="custom-args-form"
      >
        <NInput
          v-model:value="localCustomArgs"
          type="textarea"
          :rows="2"
          placeholder="如 --extra-args=... --debug"
          @blur="updateCustomArgs"
        />
      </NFormItem>
    </NForm>
  </NCard>
</template>

<style scoped>
.basic-params {
  margin-bottom: 16px;
}

.header-title {
  font-weight: 600;
}

.empty-state {
  padding: 16px;
  text-align: center;
}

.hint {
  color: var(--app-text-muted, #999);
  font-size: 13px;
}

.form-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
  gap: 12px 16px;
}

.lan-warn {
  margin-top: 12px;
}

.custom-args-form {
  margin-top: 12px;
}
</style>
