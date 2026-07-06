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

import { computed, ref } from "vue";
import { NCard, NButton, NSpin, NTooltip, NTag, NSpace } from "naive-ui";
import { useEnvStore } from "@/stores/env";
import { useProcessStore } from "@/stores/process";
import { useConfigStore } from "@/stores/config";
import { useToast } from "@/composables/useToast";
import RepairWizard from "@/components/settings/RepairWizard.vue";

const envStore = useEnvStore();
const processStore = useProcessStore();
const configStore = useConfigStore();
const toast = useToast();

const envInfo = computed(() => envStore.envInfo);
const refreshing = computed(() => envStore.loading);

/**
 * v3.4：GPU 列表（多卡 + 显存）
 *
 * 数据源：envStore.gpus（来自 system::gpu 检测，含 vram_mb）
 * 兼容兜底：当 gpus 尚未加载（首次启动未触发 detectGpus）时回退到 envInfo.gpu_name
 *
 * 展示格式：`型号 · 显存`（如 "GeForce RTX 4080 · 16 GB"）
 * 显存单位：>= 1024 MB 自动转 GB（保留 1 位小数），< 1024 MB 显示 MB
 */
const gpuItems = computed<
  Array<{
    key: string;
    model: string;
    vramText: string;
    tooltip: string;
  }>
>(() => {
  const gpus = envStore.gpus;
  if (gpus && gpus.length > 0) {
    return gpus.map((g, idx) => {
      const vramText = formatVram(g.vram_mb);
      const vendorLabel =
        g.vendor === "nvidia"
          ? "NVIDIA"
          : g.vendor === "amd"
            ? "AMD"
            : g.vendor === "intel"
              ? "Intel"
              : g.vendor === "apple"
                ? "Apple"
                : "Unknown";
      const detailParts: string[] = [`厂商: ${vendorLabel}`];
      if (g.driver_version) detailParts.push(`驱动: ${g.driver_version}`);
      if (g.cuda_version) detailParts.push(`CUDA: ${g.cuda_version}`);
      if (g.rocm_version) detailParts.push(`ROCm: ${g.rocm_version}`);
      detailParts.push(`显存: ${vramText}`);
      return {
        key: `${g.vendor}-${idx}-${g.model}`,
        model: g.model,
        vramText,
        tooltip: detailParts.join("\n"),
      };
    });
  }
  // 兜底：gpus 还没检测，使用 envInfo.gpu_name（旧版单字符串）
  if (envInfo.value?.gpu_name) {
    return [
      {
        key: "fallback-gpu_name",
        model: envInfo.value.gpu_name,
        vramText: "未知",
        tooltip:
          "显存未探测（请尝试点击右上角「刷新」或前往「设置 → 路径配置」触发 GPU 检测）",
      },
    ];
  }
  return [];
});

/** 格式化显存：>= 1024 MB → GB 保留 1 位小数；null → "未知" */
function formatVram(vramMb: number | null | undefined): string {
  if (vramMb == null) return "未知";
  if (vramMb >= 1024) {
    const gb = vramMb / 1024;
    // 整数 GB 不带小数（如 16 GB），非整数保留 1 位（如 15.9 GB）
    return Number.isInteger(gb) ? `${gb} GB` : `${gb.toFixed(1)} GB`;
  }
  return `${vramMb} MB`;
}

/** v2.18：监听地址 + 端口（用于运行中显示服务地址 + 打开浏览器） */
const listenHost = computed(
  () => configStore.config?.launch.listen_host ?? "127.0.0.1",
);
const listenPort = computed(
  () => configStore.config?.launch.listen_port ?? 8188,
);
const serviceUrl = computed(() => {
  // 0.0.0.0 在浏览器中替换为 127.0.0.1
  const host = listenHost.value === "0.0.0.0" ? "127.0.0.1" : listenHost.value;
  return "http://" + host + ":" + listenPort.value;
});

const processStatusText = computed(() => {
  switch (processStore.status.kind) {
    case "stopped":
      return { text: "未运行", color: "default" as const };
    case "starting":
      return { text: "启动中", color: "warning" as const };
    case "running":
      return { text: "运行中 (PID " + processStore.pid + ")", color: "success" as const };
    case "stopping":
      return { text: "停止中", color: "warning" as const };
    case "crashed":
      return { text: "已崩溃", color: "error" as const };
  }
});

/**
 * v3.2：环境就绪状态（供 StatusCard 显示，提高可发现性）
 *
 * readiness === null 时为"检测中"（灰色）；
 * readiness.ready === true 时为"就绪"（绿色）；
 * 否则为"未就绪"（红色），tooltip 显示缺失步骤。
 */
const readinessStatus = computed<{
  text: string;
  color: "default" | "success" | "error" | "warning";
  tooltip: string;
}>(() => {
  const r = envStore.readiness;
  if (r === null) {
    return {
      text: "检测中",
      color: "default",
      tooltip: "正在检测环境就绪状态...",
    };
  }
  if (r.ready) {
    return {
      text: "就绪",
      color: "success",
      tooltip: "环境已就绪，可以启动 ComfyUI",
    };
  }
  // 未就绪：tooltip 列出缺失步骤
  const labels: Record<string, string> = {
    CloneComfyUI: "克隆 ComfyUI 仓库",
    CreateVenv: "创建 Python 虚拟环境",
    InstallTorch: "安装 PyTorch",
    InstallRequirements: "安装 ComfyUI 依赖",
  };
  const missing = (r.missing_steps ?? [])
    .map((s) => labels[s.kind] ?? s.kind)
    .join("、");
  return {
    text: "未就绪",
    color: "error",
    tooltip: missing
      ? `缺失：${missing}（请前往「设置 → 路径配置」一键补装）`
      : "环境未就绪，请前往「设置 → 路径配置」",
  };
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

function openBrowser() {
  try {
    window.open(serviceUrl.value, "_blank", "noopener,noreferrer");
  } catch (e) {
    toast.error("打开浏览器失败", e);
  }
}

async function onRefresh() {
  try {
    await envStore.invalidateCache();
    toast.success("环境信息已刷新");
  } catch (e) {
    toast.error("刷新失败", e);
  }
}

/**
 * v1.8 / F36-Phase2：环境修复入口
 *
 * 当 PyTorch 未安装时，PyTorch 行右侧显示「诊断修复」按钮。
 * 点击后弹 RepairWizard 对话框，扫描环境问题并自动修复。
 *
 * 设计模式：**Façade** — 单一入口整合「诊断 + 修复」全流程
 */
const showRepairWizard = ref(false);
const torchBroken = computed(
  () => envInfo.value !== null && !envInfo.value.torch_installed,
);
function onOpenRepair() {
  showRepairWizard.value = true;
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
            <div v-if="gpuItems.length > 0" class="gpu-list">
              <div
                v-for="g in gpuItems"
                :key="g.key"
                class="gpu-row"
              >
                <span class="item-value gpu-model">{{ g.model }}</span>
                <span v-if="g.vramText !== '未知'" class="gpu-vram">· {{ g.vramText }}</span>
                <span v-else class="gpu-vram gpu-vram-unknown">· 显存未知</span>
              </div>
            </div>
            <span v-else class="item-value">未检测到</span>
          </div>
        </template>
        <span v-if="gpuItems.length > 0">
          显卡信息（通过 nvidia-smi / rocm-smi / WMI / system_profiler 检测）<br />
          <span v-for="(g, i) in gpuItems" :key="g.key">
            <template v-if="i > 0">──────────<br /></template>
            <span style="white-space: pre-line">{{ g.tooltip }}</span>
          </span>
        </span>
        <span v-else>未检测到 GPU（CPU 模式或无可用驱动）</span>
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
            <span class="item-value torch-row">
              <template v-if="envInfo.torch_installed">
                {{ envInfo.torch_version || "已安装" }}
              </template>
              <NTag v-else size="tiny" type="error">未安装</NTag>
              <NButton
                v-if="torchBroken"
                size="tiny"
                type="warning"
                class="diagnose-btn"
                @click="onOpenRepair"
              >
                诊断修复
              </NButton>
            </span>
          </div>
        </template>
        <span v-if="envInfo.torch_installed">
          ComfyUI 推理所用的 PyTorch 版本
        </span>
        <span v-else>
          PyTorch 未安装或不可用（可能环境损坏）。点击「诊断修复」自动扫描问题并修复。
        </span>
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

      <!-- v3.2：环境就绪状态（提高可发现性，引导用户去设置页补装） -->
      <NTooltip placement="top">
        <template #trigger>
          <div class="status-item">
            <span class="item-label">环境就绪</span>
            <NTag size="small" :type="readinessStatus.color">
              {{ readinessStatus.text }}
            </NTag>
          </div>
        </template>
        {{ readinessStatus.tooltip }}
      </NTooltip>

      <!-- v2.18：服务地址 + 打开浏览器（仅运行中/启动中显示） -->
      <NTooltip placement="top" v-if="processStore.status.kind === 'running' || processStore.status.kind === 'starting'">
        <template #trigger>
          <div class="status-item service-item">
            <span class="item-label">服务地址</span>
            <div class="service-row">
              <code class="service-url">{{ serviceUrl }}</code>
              <NButton size="tiny" type="primary" @click="openBrowser">
                打开
              </NButton>
            </div>
          </div>
        </template>
        ComfyUI 监听地址（可点击「打开」在浏览器中查看）
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

    <!-- v1.8 / F36-Phase2：环境修复向导对话框 -->
    <RepairWizard
      :show="showRepairWizard"
      @close="showRepairWizard = false"
      @repaired="
        async () => {
          showRepairWizard = false;
          await envStore.refresh();
          toast.success('环境已修复，可重新启动 ComfyUI');
        }
      "
    />
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

/* v2.18 服务地址行 */
.service-item {
  grid-column: span 2;
}

.service-row {
  display: flex;
  align-items: center;
  gap: 6px;
}

.service-url {
  flex: 1;
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  font-size: 12px;
  padding: 2px 6px;
  background: var(--app-bg-muted, rgba(127, 127, 127, 0.08));
  border-radius: 3px;
  color: var(--app-text-default, #555);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
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

/* v1.8 / F36-Phase2：PyTorch 行内联「诊断修复」按钮 */
.torch-row {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.diagnose-btn {
  flex-shrink: 0;
  font-weight: 500;
}
</style>
