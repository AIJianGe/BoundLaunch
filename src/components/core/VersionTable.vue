<script setup lang="ts">
/**
 * 版本列表表格组件（v3.1 / F26 决策 7：NDataTable）
 *
 * 用于在 NTab 内展示某一类（stable / prerelease）的 tag 列表。
 *
 * ## 设计模式
 * - **Presentational**：纯展示组件，不直接调用 API，通过 emit 通知父组件
 * - **Strategy**：根据 tag 是否为当前版本，渲染不同操作按钮
 *
 * ## 列
 * 1. 版本号（name + 当前版本标记）
 * 2. 提交 SHA（commit，截断前 7 位）
 * 3. 发布日期（date，格式化为 YYYY-MM-DD）
 * 4. 操作（切换按钮 / "当前" 标签）
 *
 * 详见 `PR/06-界面设计.md §5.1 核心版本页`（F26 重构）
 */

import { computed, h } from "vue";
import {
  NDataTable,
  NButton,
  NTag,
  NEllipsis,
  NEmpty,
  type DataTableColumns,
} from "naive-ui";
import type { TagInfo } from "@/api/types";

const props = withDefaults(
  defineProps<{
    /** tag 列表 */
    tags: TagInfo[];
    /** 当前版本 tag 名（null = 未在 tag 上） */
    currentVersion?: string | null;
    /** 加载中状态 */
    loading?: boolean;
    /** 禁用切换按钮（如 ComfyUI 运行中 / 切换中） */
    disabled?: boolean;
    /**
     * v3.10：引导安装默认版本（用于 NBadge 标识"自动装到的版本"）
     * - `null`：未设置（走自动规则，标识由父组件决定是否展示）
     * - 非空字符串：用户锁定，显示"默认安装"徽标
     */
    defaultVersion?: string | null;
  }>(),
  {
    currentVersion: null,
    loading: false,
    disabled: false,
    defaultVersion: null,
  },
);

const emit = defineEmits<{
  /** 用户点击切换按钮 */
  (e: "switch", tag: TagInfo): void;
}>();

/** 格式化日期为 YYYY-MM-DD */
function formatDate(dateStr: string): string {
  try {
    const d = new Date(dateStr);
    if (isNaN(d.getTime())) return dateStr;
    const y = d.getFullYear();
    const m = String(d.getMonth() + 1).padStart(2, "0");
    const day = String(d.getDate()).padStart(2, "0");
    return `${y}-${m}-${day}`;
  } catch {
    return dateStr;
  }
}

/** 截断 commit SHA 前 7 位 */
function shortCommit(sha: string): string {
  return sha.length > 7 ? sha.slice(0, 7) : sha;
}

/** 表格列定义（✅ P0-4 修复：computed 显式依赖 props.currentVersion） */
const columns = computed<DataTableColumns<TagInfo>>(() => {
  // 读取 props.currentVersion / props.defaultVersion 到局部变量，让 Vue 追踪依赖
  const cur = props.currentVersion;
  const def = props.defaultVersion;
  return [
    {
      title: "版本号",
      key: "name",
      width: 200,
      render: (row) => {
        const isCurrent = row.name === cur;
        // v3.10：是否为"默认安装"版本
        const isDefault = !!def && row.name === def;
        return h(
          "div",
          { class: "version-cell" },
          [
            h("span", { class: "version-name" }, row.name),
            isCurrent
              ? h(
                  NTag,
                  { size: "small", type: "success", class: "current-badge" },
                  () => "当前",
                )
              : null,
            isDefault
              ? h(
                  NTag,
                  { size: "small", type: "info", class: "default-badge" },
                  () => "默认安装",
                )
              : null,
          ],
        );
      },
    },
    {
      title: "提交",
      key: "commit",
      width: 100,
      render: (row) =>
        h(
          NEllipsis,
          { class: "commit-cell" },
          { default: () => shortCommit(row.commit) },
        ),
    },
    {
      title: "发布日期",
      key: "date",
      width: 120,
      render: (row) => h("span", { class: "date-cell" }, formatDate(row.date)),
    },
    {
      title: "操作",
      key: "actions",
      width: 120,
      fixed: "right",
      render: (row) => {
        const isCurrent = row.name === cur;
        if (isCurrent) {
          return h(
            NTag,
            { size: "small", type: "default", bordered: false },
            () => "已是当前",
          );
        }
        return h(
          NButton,
          {
            size: "small",
            type: "primary",
            disabled: props.disabled,
            onClick: () => emit("switch", row),
          },
          () => "切换到此版本",
        );
      },
    },
  ];
});

/** 空状态 */
const isEmpty = computed(
  () => !props.loading && props.tags.length === 0,
);
</script>

<template>
  <div class="version-table">
    <NEmpty
      v-if="isEmpty"
      description="暂无可用版本"
      size="small"
      class="empty-state"
    />
    <NDataTable
      v-else
      :columns="columns"
      :data="tags"
      :loading="loading"
      :bordered="false"
      :single-line="false"
      size="small"
      :max-height="480"
      :scroll-x="540"
      :pagination="false"
      :row-key="(row: TagInfo) => row.name"
    />
  </div>
</template>

<style scoped>
.version-table {
  width: 100%;
}

.empty-state {
  padding: 32px 0;
}

.version-cell {
  display: flex;
  align-items: center;
  gap: 8px;
}

.version-name {
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  font-weight: 600;
  font-size: 13px;
}

.current-badge {
  flex-shrink: 0;
}

.commit-cell {
  font-family: "JetBrains Mono", "Cascadia Code", Consolas, monospace;
  font-size: 12px;
  color: var(--app-text-muted, #999);
}

.date-cell {
  font-size: 12px;
  color: var(--app-text-muted, #999);
}
</style>
