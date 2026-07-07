<script setup lang="ts">
/**
 * v1.8 / F38：数据位置信息面板（精简版）
 *
 * 极简 UI：默认只显示 data/ 和 cache/ 两条路径 + 复制/打开/详情三个按钮。
 * 所有详细信息（模式、实际配置、装多份提示等）都在 `?` 工具提示里。
 *
 * 设计原则：
 * - **默认极简**：用户正常使用时不会被冗余信息打扰
 * - **详情可查**：用 `?` 弹层代替常驻显示
 * - **不在界面"教育"**：所有"提示用户怎么做"的话术全部塞 `?` 里
 *
 * 设计模式：
 * - **Presentational**：纯展示 + 几个动作（复制 / 打开 / 详情）
 * - **Repository**：通过 api/config.ts 读后端
 */

import { ref, onMounted } from "vue";
import {
  NCard,
  NButton,
  NSpace,
  NTooltip,
  useMessage,
} from "naive-ui";
import { configDataLocation, type DataLocationInfo } from "@/api/config";
import { useToast } from "@/composables/useToast";

const toast = useToast();
const message = useMessage();

const location = ref<DataLocationInfo | null>(null);
const loading = ref(false);

async function load() {
  loading.value = true;
  try {
    location.value = await configDataLocation();
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    toast.error("加载数据位置失败", msg);
  } finally {
    loading.value = false;
  }
}

async function copyPath(path: string, label: string) {
  try {
    await navigator.clipboard.writeText(path);
    message.success(`${label}已复制`);
  } catch (e) {
    message.error("复制失败");
  }
}

async function openInExplorer(path: string) {
  try {
    const url = `file:///${path.replace(/\\/g, "/")}`;
    window.open(url, "_blank");
    toast.info("已打开");
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    toast.error("打开失败", msg);
  }
}

onMounted(() => {
  load();
});
</script>

<template>
  <NCard size="small" class="data-location-panel">
    <div class="row">
      <span class="label">data</span>
      <code class="path">{{ location?.data_dir || "加载中..." }}</code>
      <NSpace size="small" align="center" :wrap="false" style="margin-left: auto">
        <NTooltip placement="top" trigger="hover">
          <template #trigger>
            <NButton size="tiny" tertiary>?</NButton>
          </template>
          <div v-if="location" class="detail-popup">
            <div class="detail-row">
              <span class="detail-label">模式：</span>
              <span>{{ location.mode_description }}</span>
            </div>
            <div v-if="location.portable_base_dir" class="detail-row">
              <span class="detail-label">基础目录：</span>
              <code>{{ location.portable_base_dir }}</code>
            </div>

            <div class="detail-section">ComfyUI 仓库</div>
            <div class="detail-row">
              <code>{{ location.comfyui_root_actual || location.comfyui_root_default }}</code>
              <span v-if="!location.comfyui_root_is_default" class="tag-modified">用户配置</span>
              <span v-else class="tag-default">默认</span>
            </div>

            <div class="detail-section">Python venv</div>
            <div class="detail-row">
              <code>{{ location.venv_path_actual || location.venv_path_default }}</code>
              <span v-if="!location.venv_path_is_default" class="tag-modified">用户配置</span>
              <span v-else class="tag-default">默认</span>
            </div>

            <div class="detail-section">模型</div>
            <div class="detail-row">
              <code v-if="location.models_path_actual">{{ location.models_path_actual }}</code>
              <code v-else class="text-muted">未配置（用 ComfyUI 默认）</code>
            </div>

            <div class="detail-hint">
              装多份：复制整个 launcher 文件夹到别处即可。<br />
              自定义位置：设置环境变量 <code>BOUND_LAUNCH_DATA_DIR</code>。
            </div>
          </div>
        </NTooltip>
        <NButton
          size="tiny"
          :disabled="!location"
          @click="location && copyPath(location.data_dir, '数据目录')"
        >
          📋
        </NButton>
        <NButton
          size="tiny"
          :disabled="!location"
          @click="location && openInExplorer(location.data_dir)"
        >
          📂
        </NButton>
      </NSpace>
    </div>
    <div class="row">
      <span class="label">cache</span>
      <code class="path">{{ location?.cache_dir || "加载中..." }}</code>
    </div>
  </NCard>
</template>

<style scoped>
.data-location-panel {
  margin-bottom: 16px;
}

.row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 0;
  font-size: 13px;
}

.label {
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  color: var(--app-text-muted, #999);
  width: 40px;
  flex-shrink: 0;
}

.path {
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  font-size: 12px;
  word-break: break-all;
  background: var(--app-code-bg, rgba(0, 0, 0, 0.04));
  padding: 2px 6px;
  border-radius: 4px;
}

.detail-popup {
  font-size: 12px;
  line-height: 1.7;
  max-width: 480px;
}

.detail-row {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.detail-row code {
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  font-size: 11px;
  word-break: break-all;
  background: rgba(255, 255, 255, 0.1);
  padding: 1px 4px;
  border-radius: 3px;
}

.detail-label {
  font-weight: 600;
  opacity: 0.7;
}

.detail-section {
  margin-top: 8px;
  margin-bottom: 2px;
  font-size: 11px;
  opacity: 0.6;
  text-transform: uppercase;
  letter-spacing: 0.5px;
}

.tag-default {
  font-size: 10px;
  padding: 1px 5px;
  border-radius: 3px;
  background: rgba(24, 160, 88, 0.15);
  color: #18a058;
}

.tag-modified {
  font-size: 10px;
  padding: 1px 5px;
  border-radius: 3px;
  background: rgba(229, 162, 60, 0.15);
  color: #e5a23c;
}

.detail-hint {
  margin-top: 10px;
  padding-top: 8px;
  border-top: 1px solid rgba(255, 255, 255, 0.1);
  font-size: 11px;
  opacity: 0.7;
  line-height: 1.6;
}

.text-muted {
  opacity: 0.6;
}
</style>
