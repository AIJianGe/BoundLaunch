<script setup lang="ts">
/**
 * 危险操作面板（红色区域）
 *
 * 详见 `PR/06-界面设计.md §5.3 设置页 - 危险操作`
 *
 * 三个操作（按危险等级递增）：
 * 1. [初始化环境] - 下载 Python + 创建 venv + 装 torch（中风险）
 * 2. [重建 venv] - 删除 venv 并重建（高风险，会清空依赖）
 * 3. [重置配置] - 恢复默认 config.toml（高风险，配置丢失）
 *
 * 行为：
 * - 每个操作执行前弹确认框
 * - 运行中执行需先停止 ComfyUI
 * - 长任务通过 toast 提示，进度详见任务中心
 *
 * 设计模式：
 * - **Command**：每个按钮封装为独立操作
 * - **State Machine**：idle / confirming / running / done / failed
 */

import { ref } from "vue";
import { NCard, NButton, NSpace, NTooltip } from "naive-ui";
import { useEnvStore } from "@/stores/env";
import { useConfigStore } from "@/stores/config";
import { useProcessStore } from "@/stores/process";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";

const envStore = useEnvStore();
const configStore = useConfigStore();
const processStore = useProcessStore();
const toast = useToast();
const confirm = useConfirm();

const running = ref<"init" | "rebuild" | "reset" | null>(null);

async function ensureStopped(action: string): Promise<boolean> {
  if (!processStore.isAlive) return true;
  const ok = await confirm.danger(
    `${action} - 停止 ComfyUI`,
    `ComfyUI 正在运行 (PID: ${processStore.pid || "?"})，此操作需要先停止进程。是否继续？`,
  );
  if (!ok) return false;
  try {
    await processStore.stop();
    return true;
  } catch (e) {
    toast.error("停止失败", e);
    return false;
  }
}

async function onInitEnv() {
  const ok = await confirm.warn(
    "初始化环境",
    "将执行：下载 Python → 创建 venv → 安装 torch → 校验。预计耗时 5-15 分钟。是否继续？",
  );
  if (!ok) return;
  if (!(await ensureStopped("初始化环境"))) return;

  running.value = "init";
  try {
    await envStore.createVenv(configStore.config?.paths.python_version || "3.11");
    if (configStore.launchMode !== "cpu") {
      await envStore.installTorch(configStore.config?.torch.cuda_version || "cu128");
    }
    await envStore.refresh();
    toast.success("环境初始化完成");
  } catch (e) {
    toast.error("初始化失败", e);
  } finally {
    running.value = null;
  }
}

async function onRebuildVenv() {
  const ok = await confirm.danger(
    "重建 venv",
    "将删除当前 venv 并按配置重建（含 torch + requirements 重装）。此操作不可逆，预计耗时 5-15 分钟。是否继续？",
  );
  if (!ok) return;
  if (!(await ensureStopped("重建 venv"))) return;

  running.value = "rebuild";
  try {
    await envStore.rebuildVenv();
    toast.success("venv 重建完成");
  } catch (e) {
    toast.error("重建失败", e);
  } finally {
    running.value = null;
  }
}

async function onResetConfig() {
  const ok = await confirm.danger(
    "重置配置",
    "将恢复 config.toml 到默认值（paths 保留）。此操作不可逆，是否继续？",
  );
  if (!ok) return;
  if (!(await ensureStopped("重置配置"))) return;

  running.value = "reset";
  try {
    await configStore.reset();
    toast.success("配置已重置");
  } catch (e) {
    toast.error("重置失败", e);
  } finally {
    running.value = null;
  }
}
</script>

<template>
  <NCard class="danger-zone" :bordered="true" size="small">
    <template #header>
      <span class="header-title danger-title">⚠️ 危险操作</span>
    </template>

    <NSpace vertical :size="12">
      <div class="danger-row">
        <div class="row-info">
          <div class="row-label">[初始化环境]</div>
          <div class="row-desc">下载 Python + 创建 venv + 装 torch</div>
        </div>
        <NTooltip placement="top">
          <template #trigger>
            <NButton
              size="small"
              type="warning"
              :loading="running === 'init'"
              :disabled="running !== null"
              @click="onInitEnv"
            >
              初始化环境
            </NButton>
          </template>
          适合首次安装或 torch 缺失时使用
        </NTooltip>
      </div>

      <div class="danger-row">
        <div class="row-info">
          <div class="row-label">[重建 venv]</div>
          <div class="row-desc">删除 venv 并按配置重建（含依赖重装）</div>
        </div>
        <NTooltip placement="top">
          <template #trigger>
            <NButton
              size="small"
              type="error"
              :loading="running === 'rebuild'"
              :disabled="running !== null"
              @click="onRebuildVenv"
            >
              重建 venv
            </NButton>
          </template>
          适合 venv 损坏或依赖冲突时使用
        </NTooltip>
      </div>

      <div class="danger-row">
        <div class="row-info">
          <div class="row-label">[重置配置]</div>
          <div class="row-desc">恢复 config.toml 到默认值（保留 paths）</div>
        </div>
        <NTooltip placement="top">
          <template #trigger>
            <NButton
              size="small"
              type="error"
              ghost
              :loading="running === 'reset'"
              :disabled="running !== null"
              @click="onResetConfig"
            >
              重置配置
            </NButton>
          </template>
          配置丢失不可逆，谨慎操作
        </NTooltip>
      </div>
    </NSpace>
  </NCard>
</template>

<style scoped>
.danger-zone {
  margin-bottom: 16px;
  border-color: var(--app-error, #d03050);
}

.danger-title {
  color: var(--app-error, #d03050);
}

.danger-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 8px 12px;
  background: var(--app-bg-soft, rgba(127, 127, 127, 0.04));
  border-radius: 4px;
}

.row-info {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.row-label {
  font-weight: 600;
  font-size: 13px;
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
}

.row-desc {
  font-size: 12px;
  color: var(--app-text-muted, #999);
}
</style>
