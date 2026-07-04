<script setup lang="ts">
/**
 * 模型路径页
 *
 * 详见 `PR/06-界面设计.md §4 模型路径页`
 *
 * 区块：
 * 1. 模式单选：默认 / 自定义根目录 / 高级（高级本期禁用）
 * 2. 根目录输入框 + [扫描子目录] + [应用]
 * 3. 子目录列表：每行 `子目录名 (文件数) [状态]`，可展开查看文件
 * 4. 缺失目录的 [创建] 按钮（前端调用 mkdir）
 * 5. 底部：yaml 生成时间提示
 *
 * 设计模式：
 * - **Facade**：整合 modelPathStore + configStore
 * - **State Machine**：模式切换 + 子目录展开状态
 */

import { ref, computed, watch, onMounted } from "vue";
import {
  NCard,
  NRadioGroup,
  NRadio,
  NButton,
  NSpace,
  NEmpty,
  NTag,
  NCollapse,
  NCollapseItem,
  NAlert,
  NSpin,
  NTooltip,
} from "naive-ui";
import { useModelPathStore } from "@/stores/modelPath";
import { useConfigStore } from "@/stores/config";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";
import FolderPicker from "@/components/FolderPicker.vue";
import type { ModelsMode, SubdirInfo } from "@/api/types";

const modelPathStore = useModelPathStore();
const configStore = useConfigStore();
const toast = useToast();
const confirm = useConfirm();

const localMode = ref<ModelsMode>("default");
const localRoot = ref("");
const scanning = ref(false);
const generating = ref(false);
const expandedNames = ref<string[]>([]);

watch(
  () => configStore.config,
  (cfg) => {
    if (cfg) {
      localMode.value = cfg.models.mode;
      localRoot.value = cfg.models.custom_root;
    }
  },
  { immediate: true },
);

const isCustom = computed(() => localMode.value === "custom_root");
const hasResult = computed(() => modelPathStore.isLoaded);
const subdirs = computed(() => modelPathStore.subdirs);

function formatSize(bytes: number): string {
  if (bytes === 0) return "0 B";
  const k = 1024;
  const sizes = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return `${(bytes / Math.pow(k, i)).toFixed(1)} ${sizes[i]}`;
}

function getSubdirStatus(subdir: SubdirInfo): { type: "success" | "warning" | "error"; label: string } {
  if (subdir.file_count > 0) {
    return { type: "success", label: `存在 · ${subdir.file_count} 个文件` };
  }
  if (subdir.file_count === 0) {
    return { type: "warning", label: "空目录" };
  }
  return { type: "error", label: `异常 · file_count=${subdir.file_count}` };
}

async function onScan(force = false) {
  if (!localRoot.value.trim()) {
    toast.error("请先填写根目录");
    return;
  }
  scanning.value = true;
  try {
    await modelPathStore.scan(localRoot.value, force);
    toast.success(`扫描完成，共 ${modelPathStore.subdirCount} 个子目录`);
  } catch (e) {
    toast.error("扫描失败", e);
  } finally {
    scanning.value = false;
  }
}

async function onApply() {
  try {
    await configStore.update({
      models: {
        mode: localMode.value,
        custom_root: localRoot.value,
      },
    });
    toast.success("配置已保存");
    // 如果是 custom_root 模式，自动生成 yaml
    if (localMode.value === "custom_root") {
      await onGenerate();
    }
  } catch (e) {
    toast.error("保存失败", e);
  }
}

async function onGenerate() {
  generating.value = true;
  try {
    const result = await modelPathStore.generate();
    toast.success(`extra_model_paths.yaml 已生成\n备份：${result.backed_up || "无"}`);
  } catch (e) {
    toast.error("生成失败", e);
  } finally {
    generating.value = false;
  }
}

async function onRemoveYaml() {
  const ok = await confirm.warn(
    "删除 yaml",
    "将删除 launcher 生成的 extra_model_paths.yaml（用户手动 yaml 保留）。是否继续？",
  );
  if (!ok) return;
  try {
    await modelPathStore.remove();
    toast.success("yaml 已删除");
  } catch (e) {
    toast.error("删除失败", e);
  }
}

async function onCreateSubdir(subdirName: string) {
  // 前端调用后端 fs API（暂用 toast 提示）
  // TODO: 后端 mkdir 命令待接入
  toast.info(`创建子目录 ${subdirName}（待后端实现 mkdir 命令）`);
}

onMounted(async () => {
  // 自动扫描（仅 custom_root 模式 + 已配置根目录）
  if (isCustom.value && localRoot.value.trim()) {
    await onScan().catch(() => {});
  }
});
</script>

<template>
  <div class="model-path-page">
    <!-- 模式选择 -->
    <NCard class="mode-card" :bordered="true" size="small">
      <template #header>
        <span class="header-title">📦 模型路径管理</span>
      </template>

      <NRadioGroup v-model:value="localMode" @update:value="onApply">
        <NSpace vertical :size="8">
          <div class="mode-option">
            <NRadio value="default">默认（ComfyUI 根目录/models）</NRadio>
          </div>
          <div class="mode-option">
            <NRadio value="custom_root">自定义根目录（推荐）</NRadio>
            <span class="mode-hint">与 ComfyUI 安装分离，方便管理</span>
          </div>
          <div class="mode-option">
            <NRadio value="advanced" disabled>高级（按类型指定，本期禁用）</NRadio>
          </div>
        </NSpace>
      </NRadioGroup>
    </NCard>

    <!-- 根目录输入 + 操作按钮 -->
    <NCard v-if="isCustom" class="root-card" :bordered="true" size="small">
      <template #header>
        <span class="header-title">📁 根目录</span>
      </template>

      <div class="root-row">
        <FolderPicker
          v-model="localRoot"
          placeholder="如 D:\AIWork\models"
          dialog-title="选择模型根目录"
          size="small"
          class="root-picker"
          @change="onScan()"
        />
        <NButton size="small" :loading="scanning" @click="onScan(false)">扫描子目录</NButton>
        <NButton size="small" :loading="scanning" @click="onScan(true)">强制刷新</NButton>
        <NButton size="small" type="primary" @click="onApply">应用</NButton>
      </div>

      <NAlert v-if="!localRoot.trim()" type="warning" :bordered="false" class="root-warn">
        请填写根目录路径
      </NAlert>
    </NCard>

    <!-- 子目录列表 -->
    <NCard v-if="isCustom" class="subdirs-card" :bordered="true" size="small">
      <template #header>
        <div class="card-header">
          <span class="header-title">
            📂 子目录列表 ({{ modelPathStore.subdirCount }})
          </span>
          <NButton
            v-if="modelPathStore.lastGenerated"
            size="tiny"
            type="warning"
            ghost
            @click="onRemoveYaml"
          >
            删除 yaml
          </NButton>
        </div>
      </template>

      <div v-if="scanning && !hasResult" class="loading">
        <NSpin size="medium" />
        <span class="hint">扫描中...</span>
      </div>

      <NEmpty
        v-else-if="!hasResult"
        description="尚未扫描，点击「扫描子目录」"
        size="small"
      />

      <NCollapse v-else v-model:expanded-names="expandedNames" arrow-placement="left">
        <NCollapseItem
          v-for="subdir in subdirs"
          :key="subdir.name"
          :name="subdir.name"
        >
          <template #header>
            <div class="subdir-row">
              <span class="subdir-name">📁 {{ subdir.name }}</span>
              <span class="subdir-count">({{ subdir.file_count }} 个文件)</span>
              <NTag size="tiny" :type="getSubdirStatus(subdir).type">
                {{ getSubdirStatus(subdir).label }}
              </NTag>
              <span v-if="subdir.total_size > 0" class="subdir-size">
                · {{ formatSize(subdir.total_size) }}
              </span>
            </div>
          </template>

          <div v-if="subdir.models.length === 0" class="empty-subdir">
            <span class="hint">空目录</span>
            <NButton size="tiny" @click="onCreateSubdir(subdir.name)">创建</NButton>
          </div>

          <div v-else class="file-list">
            <div
              v-for="file in subdir.models"
              :key="file.name"
              class="file-row"
            >
              <span class="file-icon">📄</span>
              <NTooltip placement="top">
                <template #trigger>
                  <span class="file-name">{{ file.name }}</span>
                </template>
                修改时间: {{ file.modified }}
              </NTooltip>
              <span class="file-size">{{ formatSize(file.size) }}</span>
            </div>
          </div>
        </NCollapseItem>
      </NCollapse>
    </NCard>

    <!-- yaml 状态提示 -->
    <NCard v-if="modelPathStore.lastGenerated" class="yaml-status" :bordered="true" size="small">
      <NAlert type="info" :bordered="false">
        ℹ 当前配置：extra_model_paths.yaml 由 launcher 自动生成
        <br>
        上次生成时间：{{ modelPathStore.lastGenerated }}
      </NAlert>
    </NCard>
  </div>
</template>

<style scoped>
.model-path-page {
  padding: 16px;
  max-width: 1200px;
  margin: 0 auto;
}

.mode-card,
.root-card,
.subdirs-card,
.yaml-status {
  margin-bottom: 16px;
}

.header-title {
  font-weight: 600;
}

.mode-option {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 0;
}

.mode-hint {
  font-size: 12px;
  color: var(--app-text-muted, #999);
}

.root-row {
  display: flex;
  gap: 8px;
  align-items: center;
}

.root-warn {
  margin-top: 8px;
}

.card-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.loading {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  padding: 32px 0;
}

.hint {
  font-size: 12px;
  color: var(--app-text-muted, #999);
}

.subdir-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.subdir-name {
  font-weight: 500;
}

.subdir-count {
  font-size: 12px;
  color: var(--app-text-muted, #999);
}

.subdir-size {
  font-size: 11px;
  color: var(--app-text-muted, #999);
}

.empty-subdir {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 8px;
}

.file-list {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.file-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 8px;
  border-radius: 4px;
  font-size: 13px;
}

.file-row:hover {
  background: var(--app-bg-soft, rgba(127, 127, 127, 0.04));
}

.file-icon {
  font-size: 14px;
}

.file-name {
  flex: 1;
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  word-break: break-all;
}

.file-size {
  color: var(--app-text-muted, #999);
  font-size: 12px;
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
}
</style>
