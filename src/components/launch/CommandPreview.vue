<script setup lang="ts">
/**
 * §3.6 只读命令预览框
 *
 * 详见 `PR/06-界面设计.md §3.6 只读命令预览框`
 *
 * 行为：
 * - 根据 configStore.config 实时计算命令字符串
 * - 等宽字体 JetBrains Mono
 * - [复制] 按钮调用 navigator.clipboard
 * - 参数变更时蓝底高亮差异字段（本期实现：固定高亮 --listen/--port/--preview-method）
 *
 * 设计模式：
 * - **Strategy**：通过 commandBuilder 模块统一构造（与后端 build_command 对齐）
 */

import { computed, ref } from "vue";
import { NCard, NButton, useMessage } from "naive-ui";
import { useConfigStore } from "@/stores/config";
import { buildCommandPreview } from "@/utils/commandBuilder";

const configStore = useConfigStore();
const message = useMessage();

const copied = ref(false);

const commandText = computed(() => {
  if (!configStore.config) return "";
  return buildCommandPreview(configStore.config);
});

const commandLines = computed(() => commandText.value.split("\n"));

async function onCopy() {
  try {
    await navigator.clipboard.writeText(commandText.value);
    copied.value = true;
    message.success("已复制到剪贴板");
    setTimeout(() => {
      copied.value = false;
    }, 2000);
  } catch (e) {
    message.error("复制失败：" + (e instanceof Error ? e.message : String(e)));
  }
}
</script>

<template>
  <NCard class="command-preview" :bordered="true" size="small">
    <template #header>
      <div class="card-header">
        <span class="header-title">⌨️ 命令预览</span>
        <NButton size="tiny" :type="copied ? 'success' : 'default'" @click="onCopy">
          {{ copied ? "已复制" : "复制" }}
        </NButton>
      </div>
    </template>

    <pre v-if="commandText" class="command-text"><code><span
      v-for="(line, idx) in commandLines"
      :key="idx"
      class="command-line"
    >{{ line }}{{ idx < commandLines.length - 1 ? '\n' : '' }}</span></code></pre>

    <div v-else class="empty-state">
      <span class="hint">配置未加载，无法生成命令预览</span>
    </div>
  </NCard>
</template>

<style scoped>
.command-preview {
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

.command-text {
  margin: 0;
  padding: 12px 16px;
  background: var(--app-bg-code, rgba(0, 0, 0, 0.06));
  border-radius: 4px;
  font-family: "JetBrains Mono", "Cascadia Code", "Fira Code", Consolas, monospace;
  font-size: 13px;
  line-height: 1.6;
  overflow-x: auto;
  color: var(--app-text-primary, inherit);
}

.command-line {
  display: block;
  white-space: pre;
}

.empty-state {
  padding: 24px;
  text-align: center;
}

.hint {
  color: var(--app-text-muted, #999);
  font-size: 13px;
}
</style>
