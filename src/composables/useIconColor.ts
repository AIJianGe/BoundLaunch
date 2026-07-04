/**
 * useIconColor — 从 Naive UI 主题读取当前图标颜色
 *
 * 背景：Lucide 图标用 `stroke="currentColor"`，依赖 CSS 颜色继承。
 * 在 Tauri WebView2 上，`currentColor` 在某些上下文里 paint 时被错误解析，
 * 导致图标完全不可见。修复方案：通过 `color` prop 显式把颜色传给 Lucide，
 * 让 SVG 的 `stroke` 属性是字面量（如 `stroke="#333"`），而非 `currentColor` 关键字。
 *
 * 用法：
 * ```ts
 * const { iconColor } = useIconColor();
 * <Rocket :color="iconColor" :size="20" />
 * ```
 *
 * 实现：从 Naive UI 主题变量 `--n-icon-color` 读取实际值（已由 NConfigProvider 注入）。
 */

import { computed, type ComputedRef } from "vue";

/**
 * Naive UI 主题色 CSS 变量列表（按优先级）
 *
 * 选择理由：
 * - `--n-icon-color` 是 Naive UI 标准的"图标默认色"变量
 * - `--n-text-color` 是正文色，作为兜底
 */
const CANDIDATE_VARS = [
  "--n-icon-color",
  "--n-text-color",
  "--n-action-color",
] as const;

const FALLBACK_COLOR = "#333";

/**
 * 单例：避免每个组件都创建 ref
 */
let cached: ComputedRef<string> | null = null;

export function useIconColor(): { iconColor: ComputedRef<string> } {
  if (!cached) {
    cached = computed<string>(() => {
      // 服务端渲染兜底
      if (typeof window === "undefined" || typeof getComputedStyle === "undefined") {
        return FALLBACK_COLOR;
      }
      // 从 :root 读取 Naive UI 主题色
      const root = document.documentElement;
      const style = getComputedStyle(root);
      for (const v of CANDIDATE_VARS) {
        const c = style.getPropertyValue(v).trim();
        if (c && c !== "") return c;
      }
      return FALLBACK_COLOR;
    });
  }
  return { iconColor: cached };
}
