<script setup lang="ts">
/**
 * §3.1 顶部状态卡片
 *
 * 详见 `PR/06-界面设计.md §3.1 顶部状态卡片`
 *
 * 数据来源：
 * - envStore.envInfo：GPU / CUDA / PyTorch / venv / ComfyUI 克隆状态
 * - envStore.pythonEnvStatus：venv / uv 可用性
 * - processStore.status：进程运行状态
 * - configStore.launchMode：当前运行模式
 *
 * 行为：
 * - 右上角 [刷新] 按钮触发 envStore.invalidateCache()（强制后端重新探测）
 * - 各字段 hover 显示 tooltip 说明
 */

import { computed } from "vue";
import { NCard, NButton, NSpin, NTooltip, NTag, NSpace } from "naive-ui";
import { useEnvStore } from "@/stores/env";
import { useProcessStore } from "@/stores/process";
import { useConfigStore } from "@/stores/config";
import { useToast } from "@/composables/useToast";

const envStore = useEnvStore();
const processStore = useProcessStore();
const configStore = useConfigStore();
const toast = useToast();

const envInfo = computed(() => envStore.envInfo);
const refreshing = computed(() => envStore.loading);

const processStatusText = computed(() => {
  switch (processStore.status.kind) {
    case "stopped":
      return { text: "未运行", color: "default" as const };
    case "starting":
      return { text: "启动中", color: "warning" as const };
    case "running":
      return { text: `运行中 (PID ${processStore.pid})`, color: "success" as const };
    case "stopping":
      return { text: "停止中", color: "warning" as const };
    case "crashed":
      return { text: "已崩溃", color: "error" as const };
  }
});

const launchModeText = computed(() => {
  const mode = configStore.launchMode;
  if (!mode) return "未配置";
  switch (mode) {
    case "cpu":
      return "CPU";
    case "gpu_high":
      return "GPU 高显存";
    case "gpu_low":
      return "GPU 低显存";
    case "gpu_no_vram":
      return "GPU 无显存";
    case "custom":
      return "自定义";
  }
});

async function onRefresh() {
  try {
    await envStore.invalidateCache();
    toast.success("环境信息已刷新");
  } catch (e) {
    toast.error("刷新失败", e);
  }
}
</script>

<template>
  <NCard class="status-card" :bordered="true" size="small">
    <template #header>
      <div class="card-header">
        <span class="header-title">🖥️ 设备与环境</span>
        <NButton
          size="tiny"
          :loading="refreshing"
          :disabled="refreshing"
          @click="onRefresh"
        >
          刷新
        </NButton>
      </div>
    </template>

    <div v-if="!envInfo" class="empty-state">
      <NSpin v-if="refreshing" size="small" />
      <span v-else class="empty-hint">环境信息未加载</span>
    </div>

    <div v-else class="status-grid">
      <NTooltip placement="top">
        <template #trigger>
          <div class="status-item">
            <span class="item-label">GPU</span>
            <span class="item-value">{{ envInfo.gpu_name || "未检测到" }}</span>
          </div>
        </template>
        显卡型号（通过 nvidia-smi 检测；CPU 模式或无 NVIDIA 驱动时为空）
      </NTooltip>

      <NTooltip placement="top">
        <template #trigger>
          <div class="status-item">
            <span class="item-label">CUDA</span>
            <span class="item-value">{{ envInfo.cuda_version || "N/A" }}</span>
          </div>
        </template>
        CUDA 驱动版本（仅 NVIDIA GPU 可用）
      </NTooltip>

      <NTooltip placement="top">
        <template #trigger>
          <div class="status-item">
            <span class="item-label">PyTorch</span>
            <span class="item-value">
              <template v-if="envInfo.torch_installed">
                {{ envInfo.torch_version || "已安装" }}
              </template>
              <NTag v-else size="tiny" type="error">未安装</NTag>
            </span>
          </div>
        </template>
        ComfyUI 推理所用的 PyTorch 版本
      </NTooltip>

      <NTooltip placement="top">
        <template #trigger>
          <div class="status-item">
            <span class="item-label">Python</span>
            <span class="item-value">{{ envInfo.python_version || "N/A" }}</span>
          </div>
        </template>
        venv 中检测到的 Python 版本
      </NTooltip>

      <NTooltip placement="top">
        <template #trigger>
          <div class="status-item">
            <span class="item-label">运行模式</span>
            <NTag size="small" :type="launchModeText === '未配置' ? 'warning' : 'info'">
              {{ launchModeText }}
            </NTag>
          </div>
        </template>
        当前 ComfyUI 启动时的显存策略（可在下方切换）
      </NTooltip>

      <NTooltip placement="top">
        <template #trigger>
          <div class="status-item">
            <span class="item-label">进程状态</span>
            <NTag size="small" :type="processStatusText.color">
              {{ processStatusText.text }}
            </NTag>
          </div>
        </template>
        ComfyUI 后端进程当前状态
      </NTooltip>
    </div>

    <div v-if="envInfo" class="footer-info">
      <NSpace size="small" align="center">
        <span class="footer-label">venv:</span>
        <code class="footer-value">{{ envInfo.venv_path || "-" }}</code>
      </NSpace>
      <NSpace size="small" align="center">
        <span class="footer-label">ComfyUI:</span>
        <NTag size="tiny" :type="envInfo.comfyui_cloned ? 'success' : 'warning'">
          {{ envInfo.comfyui_cloned ? "已克隆" : "未克隆" }}
        </NTag>
      </NSpace>
      <NSpace size="small" align="center">
        <span class="footer-label">最后更新:</span>
        <span class="footer-value">{{ envInfo.last_updated || "-" }}</span>
      </NSpace>
    </div>
  </NCard>
</template>

<style scoped>
.status-card {
  margin-bottom: 16px;
}

.card-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.header-title {
  font-weight: 600;
}

.empty-state {
  display: flex;
  align-items: center;
  justify-content: center;
  height: 80px;
  gap: 8px;
}

.empty-hint {
  color: var(--app-text-muted, #999);
  font-size: 13px;
}

.status-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
  gap: 12px 16px;
}

.status-item {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 6px 8px;
  border-radius: 4px;
  background: var(--app-bg-soft, rgba(127, 127, 127, 0.05));
  cursor: help;
}

.item-label {
  font-size: 11px;
  color: var(--app-text-muted, #999);
  text-transform: uppercase;
  letter-spacing: 0.5px;
}

.item-value {
  font-size: 13px;
  font-weight: 500;
  word-break: break-all;
}

.footer-info {
  display: flex;
  flex-wrap: wrap;
  gap: 16px;
  margin-top: 12px;
  padding-top: 12px;
  border-top: 1px solid var(--app-border, rgba(127, 127, 127, 0.15));
  font-size: 12px;
}

.footer-label {
  color: var(--app-text-muted, #999);
}

.footer-value {
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
}
</style>
