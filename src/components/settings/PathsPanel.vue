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

import { ref, computed, watch, onMounted } from "vue";
import {
  NCard,
  NForm,
  NFormItem,
  NSelect,
  NAlert,
  NSpace,
  NTooltip,
  NButton,
  NSpin,
} from "naive-ui";
import { useConfigStore } from "@/stores/config";
import { useEnvStore } from "@/stores/env";
import { useToast } from "@/composables/useToast";
import { useEnvInstaller } from "@/composables/useEnvInstaller";
import { useConfirm } from "@/composables/useConfirm";
import FolderPicker from "@/components/FolderPicker.vue";

const configStore = useConfigStore();
const envStore = useEnvStore();
const toast = useToast();
const { confirm: showConfirm } = useConfirm();

/** 上一保存的 venv 路径（用于切换时检测"路径真的变了"） */
let lastSavedVenv = "";
const {
  installing: installingEnv,
  currentStep: installStepText,
  installMissingSteps,
} = useEnvInstaller();

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
      // 首次加载时初始化 lastSavedVenv
      if (!lastSavedVenv) {
        lastSavedVenv = cfg.paths.venv_path;
      }
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
        // v3.0：切换 venv 路径时让用户确认
        const oldPath = lastSavedVenv;
        if (oldPath && oldPath !== value) {
          const oldPathStr = oldPath;
          const newPathStr = value;
          const confirmed = await showConfirm({
            title: "切换 venv 路径",
            content:
              "切换 venv 路径后，原 venv 中的 Python 包将不再被使用。\n\n" +
              "旧路径：" +
              oldPathStr +
              "\n新路径：" +
              newPathStr +
              "\n\n如果新路径不存在或不是有效 venv，下次启动 ComfyUI 前需要重新初始化环境。是否继续？",
            positiveText: "确认切换",
            negativeText: "取消",
          });
          if (!confirmed) {
            // 回退 localVenv 到原值
            localVenv.value = oldPath;
            return;
          }
          lastSavedVenv = value;
        } else {
          lastSavedVenv = value;
        }
        await configStore.update({ paths: { venv_path: value } });
        // 切换后立即重新做一次 readiness，让 UI 反映新 venv 状态
        try {
          await envStore.invalidateCache();
          await envStore.refresh();
          await envStore.checkReadiness();
        } catch (e) {
          console.warn("[PathsPanel] post-venv-change readiness failed:", e);
        }
      } else if (field === "python") {
        await configStore.update({ paths: { python_version: value } });
      }
    } catch (e) {
      toast.error("保存失败", e);
    }
  }, 500);
}

// v2.14：环境检测与补装
onMounted(async () => {
  // 初次进入面板时做一次 readiness 检查
  try {
    await envStore.refresh();
    await envStore.checkReadiness();
  } catch (e) {
    console.warn("[PathsPanel] initial env check failed:", e);
  }
});

/** 环境是否完全就绪（readiness.ready === true） */
const envReady = computed(() => envStore.readiness?.ready ?? false);

/** missing_steps 数量（0 = 就绪） */
const missingCount = computed(
  () => envStore.readiness?.missing_steps.length ?? 0,
);

/** missing_steps 简明描述（按 kind 翻译为中文） */
const missingStepsText = computed(() => {
  const steps = envStore.readiness?.missing_steps ?? [];
  const labels: Record<string, string> = {
    CloneComfyUI: "克隆 ComfyUI 仓库",
    CreateVenv: "创建 Python 虚拟环境",
    InstallTorch: "安装 PyTorch",
    InstallRequirements: "安装 ComfyUI 依赖",
  };
  return steps.map((s) => labels[s.kind] ?? s.kind).join("、");
});

/** 点击「一键补装」按钮 */
async function onInstallMissing() {
  await installMissingSteps();
}
</script>

<template>
  <NCard class="paths-panel" :bordered="true" size="small">
    <template #header>
      <span class="header-title">📁 路径配置</span>
    </template>

    <!-- v2.14：环境检测与补装入口 -->
    <div class="env-check-section">
      <NAlert
        v-if="envStore.isLoaded && envReady"
        type="success"
        :show-icon="true"
        :bordered="false"
        class="env-alert"
      >
        ✅ 环境已就绪，可以启动 ComfyUI
      </NAlert>
      <NAlert
        v-else-if="envStore.isLoaded && missingCount > 0"
        type="warning"
        :show-icon="true"
        :bordered="false"
        class="env-alert"
      >
        <div class="env-alert-content">
          <span>
            ⚠ 环境未完全就绪，缺失 {{ missingCount }} 项：
            <strong>{{ missingStepsText }}</strong>
          </span>
          <NButton
            size="small"
            type="warning"
            :loading="installingEnv"
            :disabled="installingEnv"
            @click="onInstallMissing"
          >
            <template #icon>
              <NSpin v-if="installingEnv" size="small" />
            </template>
            {{ installingEnv ? installStepText : "一键补装" }}
          </NButton>
        </div>
      </NAlert>
    </div>

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

.env-check-section {
  margin-bottom: 16px;
}

.env-alert {
  font-size: 13px;
}

.env-alert-content {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  flex-wrap: wrap;
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
