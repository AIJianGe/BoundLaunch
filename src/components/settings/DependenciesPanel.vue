<script setup lang="ts">
/**
 * 依赖管理面板（v3.0）
 *
 * 功能：扫描 [comfyui_root]/custom_nodes 下所有节点的 requirements.txt，
 * 检测同一 Python 包被多个自定义节点以不同版本约束引用的情况。
 *
 * 设计：
 * - **只检测不解决**：展示冲突，让用户决策
 * - **不阻塞启动**：检测失败也不影响其他功能
 * - **一键刷新**：手动触发重新扫描（用户装新节点后）
 *
 * 详见 PR/03-模块设计/07-EnvironmentInspector.md §13 依赖冲突检测
 */
import { ref, computed, onMounted } from "vue";
import {
  NCard,
  NButton,
  NSpin,
  NAlert,
  NSpace,
  NTag,
  NCollapse,
  NCollapseItem,
  NEmpty,
  NList,
  NListItem,
  NThing,
  NIcon,
} from "naive-ui";
import { useEnvStore } from "@/stores/env";
import { useToast } from "@/composables/useToast";
import type { Conflict, ConflictSeverity } from "@/api/types";

const envStore = useEnvStore();
const toast = useToast();

const scanning = ref(false);

/** 初始化时 + 手动刷新时调用 */
async function loadReport() {
  scanning.value = true;
  try {
    await envStore.checkConflicts();
  } finally {
    scanning.value = false;
  }
}

onMounted(loadReport);

const report = computed(() => envStore.conflictReport);
const conflicts = computed(() => report.value?.conflicts ?? []);
const hasConflict = computed(() => !report.value?.clean);

const majorCount = computed(
  () => conflicts.value.filter((c) => c.severity === "major").length,
);
const minorCount = computed(
  () => conflicts.value.filter((c) => c.severity === "minor").length,
);
const patchCount = computed(
  () => conflicts.value.filter((c) => c.severity === "patch").length,
);

/** 计算单个冲突的 collapse 标题（避免模板内反引号拼接） */
function constraintsTitle(c: Conflict): string {
  return "查看详细约束（" + c.constraints.length + " 个）";
}

/** 严重度 → UI 标签 */
function severityTagType(s: ConflictSeverity) {
  switch (s) {
    case "major":
      return "error" as const;
    case "minor":
      return "warning" as const;
    case "patch":
      return "info" as const;
  }
}

function severityLabel(s: ConflictSeverity) {
  switch (s) {
    case "major":
      return "主版本冲突";
    case "minor":
      return "范围冲突";
    case "patch":
      return "小版本冲突";
  }
}
</script>

<template>
  <NCard class="deps-panel" :bordered="false">
    <template #header>
      <div class="card-header">
        <span class="header-icon">📦</span>
        <span class="header-title">依赖管理</span>
        <NButton
          size="tiny"
          quaternary
          :loading="scanning"
          @click="loadReport"
        >
          重新扫描
        </NButton>
      </div>
    </template>

    <NSpin :show="scanning && !report">
      <!-- 未扫描 / 无 custom_nodes 目录 -->
      <NEmpty
        v-if="report && report.scanned_nodes.length === 0"
        description="未发现自定义节点，无需检查依赖冲突"
      />

      <!-- 无冲突 -->
      <NAlert
        v-else-if="report && !hasConflict"
        type="success"
        :show-icon="true"
        :bordered="false"
        class="status-alert"
      >
        ✅ 已扫描 {{ report.scanned_nodes.length }} 个自定义节点，共
        {{ report.unique_packages }} 个依赖包，未发现版本冲突。
        <div class="scan-meta">
          扫描耗时 {{ report.scan_duration_ms }}ms · 节点：
          {{ report.scanned_nodes.join("、") }}
        </div>
      </NAlert>

      <!-- 有冲突 -->
      <div v-else-if="hasConflict" class="conflicts-content">
        <NAlert
          type="warning"
          :show-icon="true"
          :bordered="false"
          class="status-alert"
        >
          ⚠ 已扫描 {{ report?.scanned_nodes.length }} 个自定义节点，
          检测到 <strong>{{ conflicts.length }}</strong> 个版本冲突。
          <NSpace size="small" class="severity-tags">
            <NTag
              v-if="majorCount > 0"
              :type="severityTagType('major')"
              size="small"
              round
            >
              主版本 {{ majorCount }}
            </NTag>
            <NTag
              v-if="minorCount > 0"
              :type="severityTagType('minor')"
              size="small"
              round
            >
              范围 {{ minorCount }}
            </NTag>
            <NTag
              v-if="patchCount > 0"
              :type="severityTagType('patch')"
              size="small"
              round
            >
              小版本 {{ patchCount }}
            </NTag>
          </NSpace>
        </NAlert>

        <NList class="conflict-list" hoverable clickable>
          <NListItem v-for="c in conflicts" :key="c.name">
            <NThing>
              <template #header>
                <div class="conflict-header">
                  <span class="pkg-name">{{ c.name }}</span>
                  <NTag :type="severityTagType(c.severity)" size="small" round>
                    {{ severityLabel(c.severity) }}
                  </NTag>
                </div>
              </template>
              <template #description>
                <div class="conflict-detail">
                  <div class="suggestion">
                    💡 {{ c.suggestion }}
                  </div>
                  <NCollapse :trigger-areas="['main']" class="constraints-collapse">
                    <NCollapseItem :title="constraintsTitle(c)">
                      <NList>
                        <NListItem
                          v-for="(cc, idx) in c.constraints"
                          :key="idx"
                          class="constraint-item"
                        >
                          <NThing :description="cc.source_file">
                            <template #header>
                              <span class="constraint-text">
                                <strong>{{ cc.node_name }}</strong>
                                要求
                                <code class="constraint-code">
                                  {{ cc.name }}{{ cc.constraint || "（无版本约束）" }}
                                </code>
                              </span>
                            </template>
                          </NThing>
                        </NListItem>
                      </NList>
                      <div class="affected-nodes">
                        受影响节点（{{ c.affected_nodes.length }}）：
                        <NTag
                          v-for="n in c.affected_nodes"
                          :key="n"
                          size="small"
                          :bordered="false"
                          class="node-tag"
                        >
                          {{ n }}
                        </NTag>
                      </div>
                    </NCollapseItem>
                  </NCollapse>
                </div>
              </template>
            </NThing>
          </NListItem>
        </NList>

        <NAlert type="info" :bordered="false" class="help-alert">
          <strong>如何解决？</strong>
          <ol class="help-list">
            <li>主版本冲突：通常需要升级/降级某个节点，或 fork 改 requirements.txt</li>
            <li>范围冲突：pip 会自动选最高版本，一般无需干预</li>
            <li>小版本冲突：pip 会自动选最高版本，无影响</li>
            <li>
              解决后请在冲突的节点目录下执行
              <code>pip install &lt;package&gt;==&lt;version&gt;</code>
            </li>
          </ol>
        </NAlert>
      </div>
    </NSpin>
  </NCard>
</template>

<style scoped>
.deps-panel {
  margin-bottom: 16px;
}

.card-header {
  display: flex;
  align-items: center;
  gap: 8px;
}

.header-icon {
  font-size: 18px;
}

.header-title {
  font-weight: 600;
  font-size: 16px;
  flex: 1;
}

.status-alert {
  margin-bottom: 16px;
}

.scan-meta {
  margin-top: 4px;
  font-size: 12px;
  opacity: 0.7;
}

.severity-tags {
  margin-top: 8px;
  display: inline-flex;
}

.conflict-list {
  margin-top: 8px;
}

.conflict-header {
  display: flex;
  align-items: center;
  gap: 8px;
}

.pkg-name {
  font-family: monospace;
  font-weight: 600;
  font-size: 14px;
}

.conflict-detail {
  padding-top: 4px;
}

.suggestion {
  margin-bottom: 8px;
  font-size: 13px;
  line-height: 1.5;
}

.constraints-collapse {
  margin-top: 8px;
}

.constraint-item {
  padding: 4px 0;
}

.constraint-text {
  font-size: 13px;
}

.constraint-code {
  background: var(--app-bg-muted, #f5f5f5);
  padding: 2px 6px;
  border-radius: 4px;
  font-family: monospace;
  font-size: 12px;
}

.affected-nodes {
  margin-top: 8px;
  font-size: 12px;
}

.node-tag {
  margin: 2px 4px 2px 0;
}

.help-alert {
  margin-top: 16px;
}

.help-list {
  margin: 8px 0 0 20px;
  padding: 0;
  font-size: 13px;
  line-height: 1.6;
}
</style>
