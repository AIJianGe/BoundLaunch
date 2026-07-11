<!--
  HardwareChangeDialog - 硬件变化检测弹窗

  v3.x Phase 3：在启动后延迟 5-10s 探测硬件变化。
  弹窗显示：
  - 当前 vs 上次的 GPU 列表 / 驱动版本
  - 推荐的应对动作（reinstall_torch / optional / no_action）
  - 3 个选项：重新安装 / 忽略（去设置手动改） / 取消

  设计原则：
  - **不阻塞主流程**：检测失败（无 GPU）静默退出
  - **首次记录不弹窗**：报告 has_change = false 时不显示
  - **可关闭**：用户选"忽略"则记住本次 session 不再弹
-->
<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { NAlert, NList, NListItem, NSpace, NTag, NText } from "naive-ui";
import { useDialog, useMessage } from "naive-ui";
import { storeToRefs } from "pinia";
import { useConfigStore } from "@/stores/config";
import type { HardwareChangeReport } from "@/api/types";
import { systemCheckVenvTorchConsistency } from "@/api/env";
import router from "@/router";

/**
 * 弹窗状态：
 * - `null`：未显示
 * - `report`：显示 + 报告数据
 */
const report = ref<HardwareChangeReport | null>(null);
const showDialog = ref(false);

const configStore = useConfigStore();
const { config } = storeToRefs(configStore);

const dialog = useDialog();
const message = useMessage();

const isReinstallRecommended = computed(() => {
  if (!report.value) return false;
  return report.value.recommended_action === "reinstall_torch";
});

const isOptional = computed(() => {
  return report.value?.recommended_action === "optional";
});

const title = computed(() => {
  if (!report.value) return "硬件变化";
  return isReinstallRecommended.value
    ? "检测到硬件变化，建议重新安装 PyTorch"
    : "检测到驱动版本变化";
});

const severity = computed<"error" | "warning" | "info">(() => {
  if (isReinstallRecommended.value) return "error";
  if (isOptional.value) return "warning";
  return "info";
});

/**
 * 触发检测 → 根据结果决定是否弹窗
 *
 * 调用方：App.vue onMounted（延迟 5-10s 后）
 */
async function check() {
  try {
    // 用动态 import 避免循环依赖
    const { systemCheckHardwareChange } = await import("@/api/env");
    const result = await systemCheckHardwareChange();

    if (!result.has_change) {
      // 无变化 / 首次记录 → 不弹窗
      return;
    }

    report.value = result;
    showDialog.value = true;
  } catch (err) {
    console.warn("[HardwareChange] 检测失败（不阻塞）:", err);
  }
}

/**
 * 选项 1：去设置 → torch 面板
 */
function goToSettings() {
  showDialog.value = false;
  router.push({ name: "settings" });
}

/**
 * 选项 2：忽略（本次 session 不再弹）
 */
function dismiss() {
  showDialog.value = false;
  message.info("已忽略，可在「设置 → torch」中手动切换");
}

/**
 * 选项 3：自动检测 venv 一致性 + 跳到设置
 * （避免在弹窗里直接触发重装，重装是长任务）
 */
async function diagnoseAndGo() {
  showDialog.value = false;

  // 探测 venv 里 torch 与配置是否一致
  try {
    const venvPython = config.value?.paths?.venv_path
      ? `${config.value.paths.venv_path}/Scripts/python.exe`
      : "";
    const configuredCuda = config.value?.torch?.cuda_version ?? "cpu";

    if (venvPython) {
      const consistency = await systemCheckVenvTorchConsistency(venvPython, configuredCuda);
      if (consistency && consistency.ok === false) {
        dialog.warning({
          title: "venv 中的 torch 与配置不一致",
          content: consistency.reason + "\n\n将跳转到「设置 → torch」面板进行重装。",
          positiveText: "去设置",
          negativeText: "取消",
          onPositiveClick: () => goToSettings(),
        });
        return;
      }
    }
  } catch (err) {
    console.warn("[HardwareChange] venv 一致性探测失败:", err);
  }

  // 一致 / 探测失败 → 直接去设置
  goToSettings();
}

defineExpose({ check });
</script>

<template>
  <n-modal
    v-model:show="showDialog"
    :mask-closable="false"
    preset="card"
    style="max-width: 600px"
    :title="title"
  >
    <n-alert
      v-if="report"
      :type="severity"
      :title="isReinstallRecommended ? 'GPU 列表发生变化' : '驱动版本发生变化'"
      style="margin-bottom: 16px"
    >
      <n-text>
        检测到当前硬件与上次记录不一致。已安装的 PyTorch 可能无法充分发挥新硬件性能。
      </n-text>
    </n-alert>

    <n-list v-if="report" bordered>
      <n-list-item>
        <n-thing title="当前 GPU">
          <n-space>
            <n-tag
              v-for="model in report.current.gpu_models"
              :key="model"
              type="success"
            >
              {{ model }}
            </n-tag>
            <n-tag v-if="report.current.gpu_models.length === 0" type="default">
              未检测到 GPU
            </n-tag>
          </n-space>
        </n-thing>
      </n-list-item>

      <n-list-item v-if="report.current.nvidia_driver">
        <n-thing title="当前 NVIDIA 驱动">
          <n-tag type="info">{{ report.current.nvidia_driver }}</n-tag>
        </n-thing>
      </n-list-item>

      <n-list-item v-if="report.previous">
        <n-thing title="上次记录的 GPU">
          <n-space>
            <n-tag
              v-for="model in report.previous.gpu_models"
              :key="model"
              type="default"
            >
              {{ model }}
            </n-tag>
            <n-tag v-if="report.previous.gpu_models.length === 0" type="default">
              未记录
            </n-tag>
          </n-space>
        </n-thing>
      </n-list-item>

      <n-list-item v-if="report.previous?.nvidia_driver">
        <n-thing title="上次 NVIDIA 驱动">
          <n-tag type="default">{{ report.previous.nvidia_driver }}</n-tag>
        </n-thing>
      </n-list-item>

      <n-list-item v-if="report.notes.length > 0">
        <n-thing title="诊断信息">
          <ul style="margin: 0; padding-left: 20px">
            <li v-for="(note, idx) in report.notes" :key="idx">
              <n-text depth="2">{{ note }}</n-text>
            </li>
          </ul>
        </n-thing>
      </n-list-item>
    </n-list>

    <template #footer>
      <n-space justify="end">
        <n-button @click="dismiss">忽略</n-button>
        <n-button @click="goToSettings">去设置</n-button>
        <n-button
          v-if="isReinstallRecommended || isOptional"
          type="primary"
          @click="diagnoseAndGo"
        >
          自动检测并去重装
        </n-button>
      </n-space>
    </template>
  </n-modal>
</template>

<style scoped>
ul {
  list-style-type: disc;
}
</style>
