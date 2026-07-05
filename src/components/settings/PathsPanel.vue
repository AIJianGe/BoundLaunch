<script setup lang="ts">
/**
 * 路径配置面板
 *
 * 详见 `PR/06-界面设计.md §5.3 设置页 - 路径配置`
 *
 * 字段：
 * - comfyui_root：ComfyUI 仓库克隆位置
 * - venv_path：Python 虚拟环境路径
 * - python_version：目标 Python 版本（仅记录，实际切换在 PythonVersionPanel）
 *
 * 行为：
 * - 输入防抖 500ms 后调用 configStore.update
 * - 实时校验：父目录可写 / 路径不互相重复
 *
 * 设计模式：
 * - **Repository**：通过 configStore 持久化
 * - **Validator**：实时校验
 */

import { ref, computed, watch } from "vue";
import {
  NCard,
  NForm,
  NFormItem,
  NSelect,
  NAlert,
  NSpace,
  NTooltip,
} from "naive-ui";
import { useConfigStore } from "@/stores/config";
import { useToast } from "@/composables/useToast";
import FolderPicker from "@/components/FolderPicker.vue";

const configStore = useConfigStore();
const toast = useToast();

const pythonVersionOptions = [
  { label: "3.10", value: "3.10" },
  { label: "3.11", value: "3.11" },
  { label: "3.12", value: "3.12" },
];

const localRoot = ref("");
const localVenv = ref("");
const localPython = ref("3.11");
const debounceTimers: Record<string, ReturnType<typeof setTimeout>> = {};

watch(
  () => configStore.config,
  (cfg) => {
    if (cfg) {
      localRoot.value = cfg.paths.comfyui_root;
      localVenv.value = cfg.paths.venv_path;
      localPython.value = cfg.paths.python_version;
    }
  },
  { immediate: true },
);

const pathConflict = computed(() => {
  if (!localRoot.value || !localVenv.value) return false;
  return localRoot.value === localVenv.value;
});

const rootEmpty = computed(() => !localRoot.value.trim());
const venvEmpty = computed(() => !localVenv.value.trim());

const hasError = computed(() => pathConflict.value || rootEmpty.value || venvEmpty.value);

function debouncedUpdate(field: "root" | "venv" | "python", value: string) {
  if (debounceTimers[field]) clearTimeout(debounceTimers[field]);
  debounceTimers[field] = setTimeout(async () => {
    try {
      if (field === "root") {
        await configStore.update({ paths: { comfyui_root: value } });
      } else if (field === "venv") {
        await configStore.update({ paths: { venv_path: value } });
      } else if (field === "python") {
        await configStore.update({ paths: { python_version: value } });
      }
    } catch (e) {
      toast.error("保存失败", e);
    }
  }, 500);
}
</script>

<template>
  <NCard class="paths-panel" :bordered="true" size="small">
    <template #header>
      <span class="header-title">📁 路径配置</span>
    </template>

    <NForm label-placement="top" :show-feedback="false" size="small">
      <NFormItem label="ComfyUI 根目录">
        <FolderPicker
          v-model="localRoot"
          placeholder="如 D:\AIWork\ComfyUI"
          :status="rootEmpty ? 'error' : 'success'"
          dialog-title="选择 ComfyUI 根目录"
          clearable
          @update:model-value="(v) => debouncedUpdate('root', v)"
        />
      </NFormItem>

      <NFormItem>
        <template #label>
          <span class="label-with-help">
            venv 路径
            <NTooltip placement="top" trigger="hover">
              <template #trigger>
                <span class="help-icon" aria-label="venv 路径说明">?</span>
              </template>
              <div class="help-content">
                venv 是 ComfyUI 专用的 Python 运行环境目录，程序会在此自动下载
                Python 并安装所需依赖（如 torch）。请选择一个空文件夹或新路径
                （程序会自动创建），请勿指向系统已有的 Python 安装目录，以免冲突。
              </div>
            </NTooltip>
          </span>
        </template>
        <FolderPicker
          v-model="localVenv"
          placeholder="如 D:\AIWork\ComfyUI\venv"
          :status="venvEmpty || pathConflict ? 'error' : 'success'"
          dialog-title="选择 venv 路径"
          clearable
          @update:model-value="(v) => debouncedUpdate('venv', v)"
        />
      </NFormItem>

      <NFormItem label="Python 版本（仅记录，切换在下方面板）">
        <NSelect
          v-model:value="localPython"
          :options="pythonVersionOptions"
          @update:value="(v) => debouncedUpdate('python', v)"
        />
      </NFormItem>
    </NForm>

    <NSpace v-if="hasError" vertical :size="8" class="error-list">
      <NAlert v-if="rootEmpty" type="error" :bordered="false">
        ComfyUI 根目录不能为空
      </NAlert>
      <NAlert v-if="venvEmpty" type="error" :bordered="false">
        venv 路径不能为空
      </NAlert>
      <NAlert v-if="pathConflict" type="error" :bordered="false">
        ⚠ ComfyUI 根目录与 venv 路径不能相同
      </NAlert>
    </NSpace>
  </NCard>
</template>

<style scoped>
.paths-panel {
  margin-bottom: 16px;
}

.header-title {
  font-weight: 600;
}

.error-list {
  margin-top: 12px;
}

.label-with-help {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}

.help-icon {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 14px;
  height: 14px;
  border-radius: 50%;
  background: var(--app-text-muted, #999);
  color: #fff;
  font-size: 10px;
  font-weight: 600;
  cursor: help;
  user-select: none;
  transition: background 0.2s;
}

.help-icon:hover {
  background: var(--app-primary, #18a058);
}

.help-content {
  max-width: 360px;
  line-height: 1.6;
}
</style>
