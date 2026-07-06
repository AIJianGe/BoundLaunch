<script setup lang="ts">
/**
 * 启动页（首页容器）
 *
 * 详见 `PR/06-界面设计.md §3 启动页（首页）`
 *
 * 8 区块编排（v2.18 合并：服务状态并入 StatusCard，移除 RunningStatusPanel）：
 * 1. §3.1 顶部状态卡片      StatusCard（含服务状态 + 打开浏览器按钮）
 * 2. §3.2 启动/停止按钮       StartStopButtons
 * 3. §3.3 运行模式单选        LaunchModeSelector
 * 4. §3.4 基础参数表单        BasicParamsForm
 * 5. §3.5 关键依赖列表        DependencyList
 * 6. §3.6 命令预览            CommandPreview
 * 7. §3.7 高级参数折叠        AdvancedParamsPanel
 * （v2.18 移除）§3.8 启动后状态 RunningStatusPanel（合并到 §3.1）
 *
 * 容器职责：
 * - 加载初始 Config / envInfo / 历史日志
 * - 编排子组件
 * - 不直接处理业务逻辑（交由子组件 + store）
 *
 * 设计模式：
 * - **Facade**：本页面是「启动页」外观，对内编排 8 个子组件
 * - **Repository**：通过 store 访问后端，不直接调用 API
 */

import { computed, onMounted, onUnmounted, ref } from "vue";
import { NSpin, NAlert, NButton } from "naive-ui";
import { useConfigStore } from "@/stores/config";
import { useEnvStore } from "@/stores/env";
import { useProcessStore } from "@/stores/process";
import { useCoreStore } from "@/stores/core";
import { useToast } from "@/composables/useToast";
import StatusCard from "@/components/launch/StatusCard.vue";
import StartStopButtons from "@/components/launch/StartStopButtons.vue";
import LaunchModeSelector from "@/components/launch/LaunchModeSelector.vue";
import BasicParamsForm from "@/components/launch/BasicParamsForm.vue";
import DependencyList from "@/components/launch/DependencyList.vue";
import CommandPreview from "@/components/launch/CommandPreview.vue";
import AdvancedParamsPanel from "@/components/launch/AdvancedParamsPanel.vue";
// v3.2.2：实时终端面板（订阅 comfyui_log 事件）
import TerminalPanel from "@/components/launch/TerminalPanel.vue";
// v2.18：服务状态合并到 StatusCard，移除 RunningStatusPanel
// import RunningStatusPanel from "@/components/launch/RunningStatusPanel.vue";

const configStore = useConfigStore();
const envStore = useEnvStore();
const processStore = useProcessStore();
const coreStore = useCoreStore();
const toast = useToast();

/** ComfyUI 仓库克隆状态（用于顶部提示） */
const cloneState = ref<"idle" | "checking" | "cloning" | "ok" | "error">("idle");
const cloneMessage = ref("");

onMounted(async () => {
  // 并行加载初始数据（subscribe 已在 App.vue 调用，此处仅补数据）
  const tasks: Promise<unknown>[] = [];

  if (!configStore.isLoaded) {
    tasks.push(configStore.load().catch((e) => console.warn("config load:", e)));
  }

  if (!envStore.isLoaded) {
    tasks.push(envStore.refresh().catch((e) => console.warn("env refresh:", e)));
  }

  tasks.push(processStore.loadHistoryLogs(200).catch((e) => console.warn("history logs:", e)));

  await Promise.allSettled(tasks);

  // 显式触发 readiness 检查（即便 env 已 loaded，refresh 内部 fire-and-forget，
  // 重新调用一次确保 UI 拿到状态）
  envStore.checkReadiness().catch((e) => console.warn("readiness:", e));

  // 自动确保 ComfyUI 仓库已克隆（首次访问 / 跳过向导场景）
  void ensureComfyUICloned();
});

/** 自动确保 ComfyUI 仓库已克隆 */
async function ensureComfyUICloned() {
  cloneState.value = "checking";
  cloneMessage.value = "检查 ComfyUI 仓库...";
  try {
    // 先获取当前状态
    if (!coreStore.status) {
      await coreStore.refresh();
    }
    // 已是仓库 → 跳过
    if (coreStore.isCloned) {
      cloneState.value = "ok";
      cloneMessage.value = "ComfyUI 仓库已就绪";
      return;
    }
    // 未克隆 → 触发自动 clone
    cloneState.value = "cloning";
    cloneMessage.value = "正在克隆 ComfyUI 仓库（约 100MB，需 1-3 分钟）...";
    await coreStore.ensureCloned();
    cloneState.value = "ok";
    cloneMessage.value = "ComfyUI 仓库已就绪";
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    // NotEmptyDir 是预期错误（用户用非空目录），给明确提示
    if (msg.includes("NotEmptyDir")) {
      cloneState.value = "error";
      cloneMessage.value = "ComfyUI 根目录已存在但不是 ComfyUI 仓库，请到设置页更换目录";
    } else {
      cloneState.value = "error";
      cloneMessage.value = `ComfyUI 仓库克隆失败：${msg}`;
      toast.error("ComfyUI 仓库克隆失败", e);
    }
  }
}

onUnmounted(() => {
  // 切换页面时清空日志缓冲（避免内存占用；下次进入会重新拉取）
  // 注意：subscribe 在 App.vue 全局订阅，此处不 unsubscribe
  // processStore.clearLogs(); // 暂不清空，便于用户切换页面回来还能看到
});

/** 初始加载状态（config 与 env 都未加载时显示 loading） */
const initialLoading = computed(
  () => !configStore.isLoaded && !envStore.isLoaded,
);
</script>

<template>
  <div class="launch-page">
    <div v-if="initialLoading" class="page-loading">
      <NSpin size="medium" />
      <span class="loading-text">加载启动页...</span>
    </div>

    <template v-else>
      <!-- ComfyUI 仓库状态提示（首次访问 / 跳过向导场景） -->
      <NAlert
        v-if="cloneState === 'cloning' || cloneState === 'checking'"
        type="info"
        :bordered="false"
        class="clone-alert"
      >
        <NSpin size="small" />
        <span style="margin-left: 8px">{{ cloneMessage }}</span>
      </NAlert>
      <NAlert
        v-else-if="cloneState === 'error'"
        type="warning"
        :bordered="false"
        class="clone-alert"
      >
        {{ cloneMessage }}
        <NButton text type="primary" @click="ensureComfyUICloned" style="margin-left: 8px">
          重试
        </NButton>
      </NAlert>

      <!-- §3.1 顶部状态卡片 -->
      <StatusCard />

      <!-- §3.2 启动 / 停止按钮 -->
      <StartStopButtons />

      <!-- §3.3 运行模式单选 -->
      <LaunchModeSelector />

      <!-- §3.4 基础参数表单 -->
      <BasicParamsForm />

      <!-- §3.5 关键依赖列表 -->
      <DependencyList />

      <!-- §3.6 命令预览 -->
      <CommandPreview />

      <!-- §3.7 高级参数折叠 -->
      <AdvancedParamsPanel />

      <!-- v3.2.2：实时终端面板（订阅 comfyui_log 事件，显示 ComfyUI stdout/stderr） -->
      <TerminalPanel />

      <!-- v2.18：§3.8 启动后状态合并到 StatusCard，不再单独渲染 -->
      <!-- <RunningStatusPanel /> -->
    </template>
  </div>
</template>

<style scoped>
.launch-page {
  padding: 16px;
  max-width: 1200px;
  margin: 0 auto;
}

.page-loading {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 12px;
  padding: 80px 0;
}

.loading-text {
  font-size: 13px;
  color: var(--app-text-muted, #999);
}

.clone-alert {
  margin-bottom: 12px;
}
</style>
