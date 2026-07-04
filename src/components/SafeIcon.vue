<script setup lang="ts">
/**
 * SafeIcon — Lucide 图标的安全包装
 *
 * 解决问题：
 * 1. Tauri WebView2 上 `stroke="currentColor"` paint 时可能渲染为 transparent
 * 2. Naive UI 主题切换时图标颜色不跟随
 *
 * 方案：
 * - 通过 `color` prop 把字面颜色传给 Lucide（让 stroke 是字面值，非 currentColor）
 * - 默认从 Naive UI 主题读取 `--n-icon-color`，主题切换自动跟随
 * - 兜底色 #333（CSS 中也有一层 fallback）
 *
 * 兼容：所有 Lucide 组件（接受 size、color、strokeWidth 等 props）
 */

import { computed } from "vue";
import { useIconColor } from "@/composables/useIconColor";

interface Props {
  /** Lucide 组件（从 @/components/icons 导入） */
  component: any;
  /** 图标尺寸（像素） */
  size?: number;
  /** 显式颜色（覆盖主题色） */
  color?: string;
  /** 描边宽度（默认 2） */
  strokeWidth?: number;
  /** 自定义 class */
  class?: string;
  /** 自定义 style */
  style?: Record<string, string | number> | string;
}

const props = withDefaults(defineProps<Props>(), {
  size: 20,
  strokeWidth: 2,
  color: undefined,
});

const { iconColor } = useIconColor();

/** 实际颜色：显式 > 主题色 */
const resolvedColor = computed(() => props.color ?? iconColor.value);
</script>

<template>
  <component
    :is="component"
    :size="size"
    :color="resolvedColor"
    :stroke-width="strokeWidth"
    :class="props.class"
    :style="props.style"
  />
</template>
