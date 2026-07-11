<!--
  GpuPanel - 多 GPU 选择面板

  v3.x Phase 5：仅支持"全部使用"和"单卡模式"
  - 全部使用 → 不设 CUDA_VISIBLE_DEVICES
  - 单卡模式 → CUDA_VISIBLE_DEVICES=<index>

  设计原则：
  - **简化决策**：不考虑 NVLink 集群等高级配置
  - **实时检测**：onMounted 时调 systemDetectGpus 拿当前 GPU 列表
  - **保存到 config**：单卡选择保存到 cfg.launch.gpu_selection，spawn 时由 process_launcher 注入环境变量
-->
<script setup lang="ts">
/**
 * GpuPanel - GPU 选择面板
 *
 * 详见 `PR/06-界面设计.md §3.x GPU 选择` (v3.x Phase 5)
 */
import { computed, onMounted, ref, watch } from "vue";
import { NCard, NSpace, NTag, NText, NButton } from "naive-ui";
import { storeToRefs } from "pinia";
import { useConfigStore } from "@/stores/config";
import { systemDetectGpus } from "@/api/env";
import { useToast } from "@/composables/useToast";
import type { GpuInfo, GpuSelectionConfig } from "@/api/types";

const configStore = useConfigStore();
const { config } = storeToRefs(configStore);
const toast = useToast();

const gpus = ref<GpuInfo[]>([]);
const loading = ref(false);
const saving = ref(false);

const selection = computed<GpuSelectionConfig>(() => {
  return (
    config.value?.launch.gpu_selection ?? { mode: "all", single_index: 0 }
  );
});

const gpuCount = computed(() => gpus.value.length);

async function refresh() {
  loading.value = true;
  try {
    gpus.value = await systemDetectGpus(true);
  } catch (err) {
    console.warn("[GpuPanel] 检测 GPU 失败:", err);
    gpus.value = [];
  } finally {
    loading.value = false;
  }
}

async function setMode(mode: "all" | "single") {
  if (!config.value) return;
  saving.value = true;
  try {
    const newSelection: GpuSelectionConfig = {
      mode,
      single_index: selection.value.single_index,
    };
    await configStore.update({
      launch: { gpu_selection: newSelection },
    });
    if (mode === "all") {
      toast.success("已设置为「全部使用」");
    }
  } catch (err) {
    toast.error("保存失败", err);
  } finally {
    saving.value = false;
  }
}

async function setSingleIndex(idx: number) {
  if (!config.value) return;
  saving.value = true;
  try {
    await configStore.update({
      launch: {
        gpu_selection: { mode: "single", single_index: idx },
      },
    });
    toast.success(`已选择 GPU [${idx}]`);
  } catch (err) {
    toast.error("保存失败", err);
  } finally {
    saving.value = false;
  }
}

onMounted(() => {
  refresh();
});

// 监听 GPU 列表变化：如果当前选中的 GPU 不存在了，自动切回 all
watch(gpus, (newGpus) => {
  if (
    selection.value.mode === "single" &&
    newGpus.length > 0 &&
    selection.value.single_index >= newGpus.length
  ) {
    setMode("all");
  }
});
</script>

<template>
  <NCard class="gpu-panel" :bordered="true" size="small">
    <template #header>
      <span class="header-title">🎮 GPU 选择</span>
    </template>

    <div v-if="loading" class="loading">检测 GPU 中...</div>

    <template v-else>
      <div v-if="gpuCount === 0" class="empty-state">
        <NText depth="3">未检测到 GPU，ComfyUI 将以 CPU 模式运行</NText>
      </div>

      <template v-else>
        <div class="gpu-list">
          <NText strong>检测到 {{ gpuCount }} 块 GPU：</NText>
          <NSpace>
            <NTag
              v-for="(g, idx) in gpus"
              :key="idx"
              :type="
                selection.mode === 'single' && selection.single_index === idx
                  ? 'success'
                  : 'default'
              "
              :class="{
                'gpu-tag-selected':
                  selection.mode === 'single' && selection.single_index === idx,
              }"
              style="cursor: pointer"
              @click="setSingleIndex(idx)"
            >
              [{{ idx }}] {{ g.model
              }}{{ g.vram_mb ? ` (${Math.round(g.vram_mb / 1024)}GB)` : "" }}
            </NTag>
          </NSpace>
        </div>

        <div class="mode-buttons">
          <NButton
            :type="selection.mode === 'all' ? 'primary' : 'default'"
            :disabled="saving"
            @click="setMode('all')"
          >
            全部使用
          </NButton>
          <NButton
            :type="selection.mode === 'single' ? 'primary' : 'default'"
            :disabled="saving"
            @click="setMode('single')"
          >
            单卡模式
          </NButton>
        </div>

        <div class="hint">
          <NText v-if="selection.mode === 'all'" depth="3" style="font-size: 12px">
            ℹ PyTorch 默认使用全部 GPU（不设 CUDA_VISIBLE_DEVICES）
          </NText>
          <NText v-else depth="3" style="font-size: 12px">
            ℹ 启动时将注入 <code>CUDA_VISIBLE_DEVICES={{ selection.single_index }}</code>
          </NText>
        </div>
      </template>
    </template>

    <div class="panel-tip">
      ℹ 多卡选择会影响 ComfyUI 能看到的 GPU 数量。复制环境到其他机器时此选择会自动失效。
    </div>
  </NCard>
</template>

<style scoped>
.gpu-panel {
  margin-bottom: 16px;
}

.header-title {
  font-weight: 600;
}

.loading,
.empty-state {
  padding: 12px;
  text-align: center;
}

.gpu-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin-bottom: 16px;
}

.mode-buttons {
  display: flex;
  gap: 8px;
  margin-bottom: 12px;
}

.hint {
  margin-top: 8px;
}

.panel-tip {
  margin-top: 12px;
  padding: 8px 12px;
  background: var(--app-bg-soft, rgba(127, 127, 127, 0.05));
  border-radius: 4px;
  font-size: 12px;
  color: var(--app-text-muted, #999);
}

code {
  background: var(--app-bg-soft, rgba(127, 127, 127, 0.1));
  padding: 1px 6px;
  border-radius: 3px;
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  font-size: 11px;
}
</style>
