<script setup lang="ts">
/**
 * PortConflictModal — 端口被占弹窗
 *
 * 设计目的：
 * - 当 ComfyUI 启动失败（端口被占）时弹出
 * - 展示占用方进程信息（PID + 名称 + 命令行）
 * - 提供"结束占用进程"快捷操作
 *
 * 触发方式：
 * - 监听 processStore.startFailedReason，reason === "port_in_use" 时显示
 *
 * 关键交互：
 * - 用户点"结束该进程" → 调 forceKillProcess(pid) → 二次确认 → 调
 * - 用户点"清理所有 Python 进程" → 调 forceKillAllPython → 二次确认
 * - 用户点"修改端口" → 跳到设置页（startup_listen_port）
 * - 用户点"取消" → 关闭弹窗
 */

import { computed, ref, watch } from "vue";
import { useRouter } from "vue-router";
import {
  NModal,
  NCard,
  NButton,
  NSpace,
  NTag,
  NAlert,
  NScrollbar,
  NIcon,
  NText,
  useDialog,
  useMessage,
} from "naive-ui";
import { useProcessStore } from "@/stores/process";
import { forceKillProcess, forceKillAllPython } from "@/api/port_diagnostics";

const router = useRouter();
const dialog = useDialog();
const message = useMessage();
const processStore = useProcessStore();

const show = computed({
  get: () => processStore.startFailedReason !== null,
  set: (val: boolean) => {
    if (!val) processStore.dismissStartFailed();
  },
});

const payload = computed(() => processStore.startFailedReason);
const diagnosis = computed(() => payload.value?.diagnosis ?? null);
const occupiedBy = computed(() => diagnosis.value?.occupied_by ?? null);

const killing = ref(false);
const killingAll = ref(false);

const isPortInUse = computed(() => payload.value?.reason === "port_in_use");

// 当弹窗打开时，记录是否已经被诊断
watch(payload, (val) => {
  if (val) {
    console.info("[PortConflictModal] opened", val);
  }
});

/** 结束占用进程（单个 PID） */
async function onKillOccupying() {
  const info = occupiedBy.value;
  if (!info) {
    message.warning("没有可结束的占用进程信息");
    return;
  }

  const confirmed = await new Promise<boolean>((resolve) => {
    dialog.warning({
      title: "确认结束进程？",
      content: `即将强制结束进程 ${info.name}（PID ${info.pid}）`,
      positiveText: "结束进程",
      negativeText: "取消",
      onPositiveClick: () => resolve(true),
      onNegativeClick: () => resolve(false),
      onClose: () => resolve(false),
    });
  });

  if (!confirmed) return;

  killing.value = true;
  try {
    await forceKillProcess(info.pid);
    message.success(`已结束进程 ${info.pid}`);
    processStore.dismissStartFailed();
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    message.error(`结束进程失败：${msg}`);
  } finally {
    killing.value = false;
  }
}

/** 清理所有 Python 进程（兜底） */
async function onKillAllPython() {
  const confirmed = await new Promise<boolean>((resolve) => {
    dialog.warning({
      title: "⚠️ 危险操作确认",
      content: "即将强制结束系统中所有 Python 进程（不限于 ComfyUI）。\n\n请确认你正在运行的 Python 程序都允许被中断。",
      positiveText: "全部结束",
      negativeText: "取消",
      onPositiveClick: () => resolve(true),
      onNegativeClick: () => resolve(false),
      onClose: () => resolve(false),
    });
  });

  if (!confirmed) return;

  killingAll.value = true;
  try {
    await forceKillAllPython();
    message.success("已结束所有 Python 进程");
    processStore.dismissStartFailed();
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    message.error(`清理 Python 进程失败：${msg}`);
  } finally {
    killingAll.value = false;
  }
}

/** 跳到设置页修改端口 */
function onChangePort() {
  processStore.dismissStartFailed();
  router.push({ name: "Launch", query: { tab: "basic" } });
}
</script>

<template>
  <NModal
    v-model:show="show"
    :mask-closable="false"
    preset="card"
    style="max-width: 600px"
    title="🚫 端口被占用"
  >
    <div v-if="payload && isPortInUse">
      <NAlert type="error" :show-icon="false" style="margin-bottom: 16px">
        端口 <strong>{{ payload.port }}</strong> 已被其他进程占用，ComfyUI 无法启动。
      </NAlert>

      <!-- 占用方信息 -->
      <NCard
        v-if="occupiedBy"
        size="small"
        :bordered="true"
        class="occupied-card"
        style="margin-bottom: 16px"
      >
        <div class="occupied-info">
          <div class="info-row">
            <NText depth="3" class="label">进程名</NText>
            <NTag type="warning" size="small" :bordered="false">
              {{ occupiedBy.name }}
            </NTag>
          </div>
          <div class="info-row">
            <NText depth="3" class="label">PID</NText>
            <NTag :bordered="false" size="small">
              {{ occupiedBy.pid }}
            </NTag>
          </div>
          <div v-if="occupiedBy.command" class="info-row command">
            <NText depth="3" class="label">命令行</NText>
            <NScrollbar x-scrollable style="max-width: 100%">
              <code class="command-text">{{ occupiedBy.command }}</code>
            </NScrollbar>
          </div>
        </div>
      </NCard>

      <NAlert v-else type="warning" :show-icon="false" style="margin-bottom: 16px">
        <div>
          <strong>无法识别占用方进程。</strong>
          <br />
          可能原因：进程已退出、权限不足、netstat 解析失败。
        </div>
      </NAlert>

      <!-- 错误消息 -->
      <NScrollbar v-if="payload.error_message" style="max-height: 120px">
        <pre class="error-detail">{{ payload.error_message }}</pre>
      </NScrollbar>
    </div>

    <div v-else-if="payload">
      <NAlert type="error" :show-icon="false">
        启动失败：{{ payload.error_message }}
      </NAlert>
    </div>

    <template #footer>
      <NSpace justify="space-between">
        <NSpace>
          <NButton
            v-if="occupiedBy"
            type="warning"
            :loading="killing"
            :disabled="killing || killingAll"
            @click="onKillOccupying"
          >
            🗑 结束该进程
          </NButton>
          <NButton
            strong
            type="error"
            :loading="killingAll"
            :disabled="killing || killingAll"
            @click="onKillAllPython"
          >
            ☠️ 清理所有 Python 进程
          </NButton>
        </NSpace>
        <NSpace>
          <NButton @click="onChangePort">
            修改 ComfyUI 端口
          </NButton>
          <NButton @click="show = false">
            关闭
          </NButton>
        </NSpace>
      </NSpace>
    </template>
  </NModal>
</template>

<style scoped>
.occupied-card {
  background: rgba(255, 165, 0, 0.05);
  border-color: rgba(255, 165, 0, 0.3);
}

.occupied-info {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.info-row {
  display: flex;
  align-items: center;
  gap: 12px;
}

.info-row .label {
  min-width: 60px;
  font-size: 12px;
}

.info-row.command {
  flex-direction: column;
  align-items: flex-start;
}

.command-text {
  font-family: "Cascadia Code", "Consolas", monospace;
  font-size: 12px;
  background: rgba(0, 0, 0, 0.05);
  padding: 6px 8px;
  border-radius: 4px;
  white-space: pre-wrap;
  word-break: break-all;
  max-width: 100%;
}

.error-detail {
  font-family: "Cascadia Code", "Consolas", monospace;
  font-size: 12px;
  background: rgba(0, 0, 0, 0.05);
  padding: 8px 10px;
  border-radius: 4px;
  white-space: pre-wrap;
  word-break: break-word;
  margin: 0;
}
</style>
