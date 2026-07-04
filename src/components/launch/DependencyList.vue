<script setup lang="ts">
/**
 * §3.5 关键依赖列表
 *
 * 详见 `PR/06-界面设计.md §3.5 关键依赖列表`
 *
 * 数据来源：
 * - envStore.dependencies：12 项关键依赖清单
 *
 * 状态标记：
 * - ✓ ok：已安装且版本匹配
 * - ⚠ outdated：版本不匹配
 * - ✗ missing：未安装
 * - ? unknown：状态未知
 */

import { computed } from "vue";
import { NCard, NButton, NEmpty, NTag, NSpin, NSpace } from "naive-ui";
import { useEnvStore } from "@/stores/env";

const envStore = useEnvStore();

const dependencies = computed(() => envStore.dependencies);
const loading = computed(() => envStore.loading);
const totalCount = computed(() => dependencies.value.length);

const statusTagConfig = computed(() => {
  return (status: string) => {
    switch (status) {
      case "ok":
        return { type: "success" as const, label: "✓", text: "已安装" };
      case "outdated":
        return { type: "warning" as const, label: "⚠", text: "版本不匹配" };
      case "missing":
        return { type: "error" as const, label: "✗", text: "未安装" };
      default:
        return { type: "default" as const, label: "?", text: "未知" };
    }
  };
});

const summary = computed(() => {
  const deps = dependencies.value;
  const ok = deps.filter((d) => d.status === "ok").length;
  const outdated = deps.filter((d) => d.status === "outdated").length;
  const missing = deps.filter((d) => d.status === "missing").length;
  return { ok, outdated, missing, total: deps.length };
});

async function onRefresh() {
  await envStore.refresh();
}
</script>

<template>
  <NCard class="dependency-list" :bordered="true" size="small">
    <template #header>
      <div class="card-header">
        <span class="header-title">📦 关键依赖</span>
        <NSpace size="small" align="center">
          <span v-if="totalCount > 0" class="summary">
            {{ summary.ok }}/{{ summary.total }} 已安装
            <span v-if="summary.outdated > 0" class="warn-count">
              · {{ summary.outdated }} 待更新
            </span>
            <span v-if="summary.missing > 0" class="error-count">
              · {{ summary.missing }} 缺失
            </span>
          </span>
          <NButton size="tiny" :loading="loading" @click="onRefresh">刷新</NButton>
        </NSpace>
      </div>
    </template>

    <div v-if="loading && totalCount === 0" class="loading-state">
      <NSpin size="small" />
      <span class="hint">加载依赖列表...</span>
    </div>

    <NEmpty
      v-else-if="totalCount === 0"
      description="暂无依赖信息，请点击「刷新」"
      size="small"
    />

    <div v-else class="dep-grid">
      <div
        v-for="dep in dependencies"
        :key="dep.name"
        class="dep-row"
        :class="`dep-${dep.status}`"
      >
        <div class="dep-name">
          <span class="dep-icon">{{ statusTagConfig(dep.status).label }}</span>
          <span class="dep-name-text">{{ dep.name }}</span>
        </div>
        <div class="dep-version">
          <span class="version-text">{{ dep.version || "未安装" }}</span>
          <NTag size="tiny" :type="statusTagConfig(dep.status).type">
            {{ statusTagConfig(dep.status).text }}
          </NTag>
        </div>
      </div>
    </div>

    <div class="legend">
      <span class="legend-item"><span class="legend-icon">✓</span> 已安装</span>
      <span class="legend-item"><span class="legend-icon warn">⚠</span> 版本不匹配</span>
      <span class="legend-item"><span class="legend-icon error">✗</span> 未安装</span>
    </div>
  </NCard>
</template>

<style scoped>
.dependency-list {
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

.summary {
  font-size: 12px;
  color: var(--app-text-muted, #999);
}

.warn-count {
  color: var(--app-warning, #f0a020);
}

.error-count {
  color: var(--app-error, #d03050);
}

.loading-state {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  padding: 24px;
}

.hint {
  font-size: 13px;
  color: var(--app-text-muted, #999);
}

.dep-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
  gap: 6px 12px;
}

.dep-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 6px 8px;
  border-radius: 4px;
  font-size: 13px;
  background: var(--app-bg-soft, rgba(127, 127, 127, 0.04));
}

.dep-row.dep-missing {
  background: rgba(208, 48, 80, 0.06);
}

.dep-row.dep-outdated {
  background: rgba(240, 160, 32, 0.06);
}

.dep-name {
  display: flex;
  align-items: center;
  gap: 6px;
}

.dep-icon {
  font-weight: 600;
  width: 14px;
  text-align: center;
}

.dep-missing .dep-icon {
  color: var(--app-error, #d03050);
}

.dep-outdated .dep-icon {
  color: var(--app-warning, #f0a020);
}

.dep-ok .dep-icon {
  color: var(--app-success, #18a058);
}

.dep-name-text {
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
}

.dep-version {
  display: flex;
  align-items: center;
  gap: 6px;
}

.version-text {
  color: var(--app-text-muted, #999);
  font-size: 12px;
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
}

.legend {
  display: flex;
  gap: 16px;
  margin-top: 12px;
  padding-top: 8px;
  border-top: 1px solid var(--app-border, rgba(127, 127, 127, 0.1));
  font-size: 11px;
  color: var(--app-text-muted, #999);
}

.legend-item {
  display: flex;
  align-items: center;
  gap: 4px;
}

.legend-icon {
  font-weight: 600;
}

.legend-icon.warn {
  color: var(--app-warning, #f0a020);
}

.legend-icon.error {
  color: var(--app-error, #d03050);
}
</style>
