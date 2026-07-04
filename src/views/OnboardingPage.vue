<script setup lang="ts">
/**
 * 首次运行向导
 *
 * 详见 `PR/06-界面设计.md §0 首次运行向导`
 *
 * 4 步向导：
 * 1. 选择 ComfyUI 根目录
 * 2. 选择 Python venv 位置
 * 3. 选择运行模式（CPU / GPU / Custom）
 * 4. 环境初始化（创建 venv + 装 torch，可选跳过）
 *
 * 设计模式：
 * - **Builder**：分步构建 Config，最后一步保存
 * - **State Machine**：currentStep 控制 4 态切换
 *
 * 行为：
 * - 任意步骤可点击「上一步」回退（保留已填数据）
 * - 最后一步「开始初始化」进入长任务（订阅 task_progress）
 * - 「跳过向导」直接进入主界面（设置页仍可改配置）
 */

import { ref, computed, reactive } from "vue";
import { useRouter } from "vue-router";
import {
  NCard,
  NSteps,
  NStep,
  NButton,
  NSpace,
  NRadioGroup,
  NRadio,
  NForm,
  NFormItem,
  NAlert,
  NProgress,
  NSpin,
} from "naive-ui";
import { useConfigStore } from "@/stores/config";
import { useEnvStore } from "@/stores/env";
import { useToast } from "@/composables/useToast";
import FolderPicker from "@/components/FolderPicker.vue";
import type { LaunchMode, CudaVersion } from "@/api/types";

const router = useRouter();
const configStore = useConfigStore();
const envStore = useEnvStore();
const toast = useToast();

// ========== State ==========
const currentStep = ref(0);
const initializing = ref(false);
const initProgress = ref(0);
const initStage = ref("");
const initError = ref<string | null>(null);

// 表单数据（与 Config 字段对应）
const form = reactive({
  comfyui_root: "",
  venv_path: "",
  python_version: "3.11",
  mode: "gpu_high" as LaunchMode,
  cuda_version: "cu121" as CudaVersion,
});

// ========== Computed ==========
const canNext = computed(() => {
  switch (currentStep.value) {
    case 0: // 根目录
      return form.comfyui_root.trim().length > 0;
    case 1: // venv
      return (
        form.venv_path.trim().length > 0 &&
        form.venv_path !== form.comfyui_root
      );
    case 2: // 模式
      return true;
    case 3: // 初始化
      return false; // 最后一步无 next
    default:
      return false;
  }
});

// ========== Actions ==========

function next() {
  if (!canNext.value) return;
  currentStep.value = Math.min(3, currentStep.value + 1);
}

function prev() {
  currentStep.value = Math.max(0, currentStep.value - 1);
}

async function saveConfig() {
  await configStore.update({
    paths: {
      comfyui_root: form.comfyui_root,
      venv_path: form.venv_path,
      python_version: form.python_version,
    },
    launch: {
      mode: form.mode,
    },
    torch: {
      cuda_version: form.cuda_version,
    },
  });
}

/** 跳过向导，进入主界面 */
async function skipOnboarding() {
  try {
    await saveConfig();
    toast.info("已跳过向导，可稍后在设置页配置");
    router.push("/launch");
  } catch (e) {
    toast.error("保存配置失败", e);
  }
}

/**
 * 完成向导（保存配置 + 可选初始化环境）
 *
 * - 用户点击「跳过初始化」：仅保存配置，跳转主界面
 * - 用户点击「开始初始化」：保存配置 + 创建 venv + 装 torch + 显示进度
 */
async function finishWithInit() {
  initializing.value = true;
  initError.value = null;
  initProgress.value = 0;

  try {
    // 步骤 1: 保存配置
    initStage.value = "保存配置中...";
    await saveConfig();
    initProgress.value = 10;

    // 步骤 2: 创建 venv
    initStage.value = "创建 Python 虚拟环境（venv）...";
    await envStore.createVenv(form.python_version);
    initProgress.value = 40;

    // 步骤 3: 安装 torch（仅 GPU 模式）
    if (form.mode !== "cpu") {
      initStage.value = `安装 PyTorch（${form.cuda_version}）...`;
      await envStore.installTorch(form.cuda_version);
    }
    initProgress.value = 80;

    // 步骤 4: 刷新环境信息
    initStage.value = "校验环境...";
    await envStore.refresh();
    initProgress.value = 100;

    toast.success("环境初始化完成");
    setTimeout(() => {
      router.push("/launch");
    }, 500);
  } catch (e) {
    initError.value = e instanceof Error ? e.message : String(e);
    toast.error("初始化失败", e);
  } finally {
    initializing.value = false;
  }
}

/** 仅保存配置，跳过环境初始化 */
async function finishWithoutInit() {
  try {
    await saveConfig();
    toast.success("配置已保存，可稍后在设置页初始化环境");
    router.push("/launch");
  } catch (e) {
    toast.error("保存配置失败", e);
  }
}
</script>

<template>
  <div class="onboarding-page">
    <NCard class="onboarding-card" :bordered="false">
      <template #header>
        <div class="card-header">
          <span class="header-icon">🚀</span>
          <span class="header-title">欢迎使用 无界启动器</span>
        </div>
      </template>

      <NSteps :current="currentStep + 1" :status="initError ? 'error' : 'process'" class="steps">
        <NStep title="ComfyUI 根目录" description="ComfyUI 仓库克隆位置" />
        <NStep title="Python 环境" description="venv 虚拟环境路径" />
        <NStep title="运行模式" description="CPU / GPU / 自定义" />
        <NStep title="环境初始化" description="创建 venv + 安装 torch" />
      </NSteps>

      <!-- 步骤 1: ComfyUI 根目录 -->
      <div v-if="currentStep === 0" class="step-content">
        <NForm label-placement="top">
          <NFormItem label="ComfyUI 根目录">
            <FolderPicker
              v-model="form.comfyui_root"
              placeholder="如 D:\AIWork\ComfyUI"
              dialog-title="选择 ComfyUI 根目录"
              clearable
              size="medium"
            />
          </NFormItem>
          <NFormItem label="Python 版本">
            <NRadioGroup v-model:value="form.python_version">
              <NRadio value="3.10">3.10</NRadio>
              <NRadio value="3.11">3.11（推荐）</NRadio>
              <NRadio value="3.12">3.12</NRadio>
            </NRadioGroup>
          </NFormItem>
        </NForm>
        <NAlert type="info" :bordered="false">
          此目录将用于克隆 ComfyUI 仓库，建议预留 5GB 磁盘空间。
        </NAlert>
      </div>

      <!-- 步骤 2: venv 路径 -->
      <div v-if="currentStep === 1" class="step-content">
        <NForm label-placement="top">
          <NFormItem label="venv 虚拟环境路径">
            <FolderPicker
              v-model="form.venv_path"
              :placeholder="form.comfyui_root ? `${form.comfyui_root}/venv` : '如 D:\\AIWork\\ComfyUI\\venv'"
              dialog-title="选择 venv 虚拟环境路径"
              clearable
              size="medium"
            />
          </NFormItem>
        </NForm>
        <NAlert
          :type="form.venv_path === form.comfyui_root ? 'warning' : 'info'"
          :bordered="false"
        >
          <span v-if="form.venv_path === form.comfyui_root">
            ⚠ venv 路径不能与 ComfyUI 根目录相同
          </span>
          <span v-else>
            venv 用于隔离 Python 依赖，建议放在 ComfyUI 根目录下的 venv 子目录。
          </span>
        </NAlert>
      </div>

      <!-- 步骤 3: 运行模式 -->
      <div v-if="currentStep === 2" class="step-content">
        <NRadioGroup v-model:value="form.mode" class="mode-group">
          <div class="mode-option">
            <NRadio value="cpu">CPU 模式（无 GPU 或仅测试）</NRadio>
            <span class="mode-hint">--cpu --lowvram</span>
          </div>
          <div class="mode-option">
            <NRadio value="gpu_high">GPU 高显存（推荐）</NRadio>
            <span class="mode-hint">--highvram</span>
          </div>
          <div class="mode-option">
            <NRadio value="gpu_low">GPU 低显存</NRadio>
            <span class="mode-hint">--lowvram</span>
          </div>
          <div class="mode-option">
            <NRadio value="gpu_no_vram">GPU 无显存</NRadio>
            <span class="mode-hint">--novram</span>
          </div>
          <div class="mode-option">
            <NRadio value="custom">自定义（高级用户）</NRadio>
            <span class="mode-hint">在设置页填写 custom_args</span>
          </div>
        </NRadioGroup>

        <NForm v-if="form.mode !== 'cpu'" label-placement="top" class="cuda-form">
          <NFormItem label="CUDA 版本">
            <NRadioGroup v-model:value="form.cuda_version">
              <NRadio value="cu118">CUDA 11.8</NRadio>
              <NRadio value="cu121">CUDA 12.1（推荐）</NRadio>
              <NRadio value="cu124">CUDA 12.4</NRadio>
            </NRadioGroup>
          </NFormItem>
        </NForm>
      </div>

      <!-- 步骤 4: 环境初始化 -->
      <div v-if="currentStep === 3" class="step-content">
        <div v-if="!initializing && !initError">
          <NAlert type="info" :bordered="false" class="init-intro">
            <p><strong>即将执行：</strong></p>
            <ol>
              <li>保存配置到 config.toml</li>
              <li>创建 Python 虚拟环境（venv）</li>
              <li v-if="form.mode !== 'cpu'">安装 PyTorch（{{ form.cuda_version }}）</li>
              <li>校验环境（verify_venv）</li>
            </ol>
            <p>预计耗时 5-15 分钟，取决于网络速度。</p>
          </NAlert>
        </div>

        <div v-if="initializing" class="init-progress">
          <NSpin size="small" />
          <div class="init-stage">{{ initStage }}</div>
          <NProgress
            type="line"
            :percentage="initProgress"
            :height="8"
            :bordered="false"
          />
        </div>

        <div v-if="initError" class="init-error">
          <NAlert type="error" :bordered="false">
            <strong>初始化失败：</strong>{{ initError }}
          </NAlert>
        </div>
      </div>

      <!-- 底部按钮区 -->
      <div class="footer-actions">
        <NButton @click="skipOnboarding" :disabled="initializing" quaternary>
          跳过向导
        </NButton>
        <NSpace>
          <NButton
            v-if="currentStep > 0 && currentStep < 3"
            @click="prev"
            :disabled="initializing"
          >
            上一步
          </NButton>
          <NButton
            v-if="currentStep < 3"
            type="primary"
            :disabled="!canNext"
            @click="next"
          >
            下一步
          </NButton>
          <NButton
            v-if="currentStep === 3"
            type="default"
            :disabled="initializing"
            @click="finishWithoutInit"
          >
            跳过初始化
          </NButton>
          <NButton
            v-if="currentStep === 3"
            type="primary"
            :loading="initializing"
            :disabled="initializing || !!initError"
            @click="finishWithInit"
          >
            开始初始化
          </NButton>
        </NSpace>
      </div>
    </NCard>
  </div>
</template>

<style scoped>
.onboarding-page {
  min-height: 100vh;
  display: flex;
  align-items: center;
  justify-content: center;
  background: var(--app-bg-muted, #f5f5f5);
  padding: 24px;
}

.onboarding-card {
  width: 640px;
  max-width: 100%;
}

.card-header {
  display: flex;
  align-items: center;
  gap: 8px;
}

.header-icon {
  font-size: 24px;
}

.header-title {
  font-weight: 600;
  font-size: 18px;
}

.steps {
  margin: 16px 0 24px;
}

.step-content {
  min-height: 180px;
  padding: 16px 0;
}

.mode-group {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.mode-option {
  display: flex;
  align-items: center;
  gap: 12px;
}

.mode-hint {
  font-size: 12px;
  opacity: 0.6;
  font-family: monospace;
}

.cuda-form {
  margin-top: 16px;
}

.init-intro ol {
  margin: 8px 0 8px 20px;
  padding: 0;
}

.init-progress {
  display: flex;
  flex-direction: column;
  gap: 12px;
  align-items: flex-start;
}

.init-stage {
  font-size: 14px;
  color: var(--app-fg, #333);
}

.init-error {
  margin-top: 12px;
}

.footer-actions {
  margin-top: 24px;
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding-top: 16px;
  border-top: 1px solid var(--app-border, #e0e0e0);
}
</style>
