<script setup lang="ts">
/**
 * 主布局
 *
 * 详见 `PR/06-界面设计.md §1 整体布局`
 *
 * 结构：
 * - 左侧：7 项导航（窄边栏 64px，仅图标 + tooltip）
 * - 顶栏：AppHeader（项目名 + 状态指示 + 设置入口）
 * - 内容区：RouterView
 *
 * 导航项（v3.4 调整顺序：日志上移到第 2 位）：
 * 1. 启动（首页）
 * 2. 日志
 * 3. 核心版本
 * 4. 插件管理
 * 5. 模型路径
 * 6. 任务进度
 * 7. 关于
 * （设置入口在顶栏右上角 AppHeader）
 *
 * v3.4 调整原因：
 * - 启动 ComfyUI 后用户最关心的就是日志（启动失败要看错误信息）
 * - 旧顺序"启动 → 核心版本"中间隔着 3 个不相关的菜单，跳转摩擦大
 * - 启动按钮默认会把页面跳转到 /logs（详见 useStartComfyui），同位置触发更顺滑
 *
 * 实现要点：
 * - 使用 NMenu 标准 `icon` + `label` props（NMenu 在 collapsed 状态下会自动管理图标/文字显隐）
 * - 自定义 `label: () => h(...)` 会触发 NMenu 给 `.n-menu-item-content-header` 设 `opacity: 0`，
 *   吞掉所有子元素（包括图标）。这是本项目踩过的坑，方案 D 改用标准 props 规避。
 * - SafeIcon wrapper 保留：把 `color` prop 显式传给 Lucide，避免 `stroke="currentColor"` 在
 *   Tauri WebView2 上 paint 时被错误解析。
 */

import {
  NLayout,
  NLayoutSider,
  NLayoutHeader,
  NLayoutContent,
  NMenu,
  NScrollbar,
  NBadge,
  type MenuOption,
} from "naive-ui";
import { computed, h, type Component } from "vue";
import { useRoute, useRouter } from "vue-router";
import AppHeader from "@/components/AppHeader.vue";
import SafeIcon from "@/components/SafeIcon.vue";
import { useErrorLog } from "@/composables/useErrorLog";
import {
  Rocket,
  RefreshCw,
  Puzzle,
  Package,
  BarChart3,
  Info,
  Terminal as TerminalIcon,
} from "@/components/icons";

const route = useRoute();
const router = useRouter();

// v3.10：业务错误 store（菜单"日志"红点）
const errorLog = useErrorLog();

interface NavItem {
  key: string;
  label: string;
  icon: Component;
  path: string;
}

const menus: readonly NavItem[] = [
  { key: "launch", label: "启动", icon: Rocket, path: "/launch" },
  { key: "logs", label: "终端", icon: TerminalIcon, path: "/logs" },
  { key: "core", label: "核心版本", icon: RefreshCw, path: "/core" },
  { key: "plugins", label: "插件管理", icon: Puzzle, path: "/plugins" },
  { key: "models", label: "模型路径", icon: Package, path: "/models" },
  { key: "tasks", label: "任务进度", icon: BarChart3, path: "/tasks" },
  { key: "about", label: "关于", icon: Info, path: "/about" },
] as const;

/**
 * 菜单选项（方案 D：使用标准 icon + label props）
 *
 * - icon: render 函数 → NMenu 在 collapsed 时只显示 icon（自动隐藏 label）
 * - label: 普通字符串 → NMenu 内部处理显隐
 *
 * v3.10：**日志菜单**图标加红点（未读错误 > 0 时显示），
 * 红点数据来自 `useErrorLog().unreadCount`。
 */
const menuOptions = computed<MenuOption[]>(() =>
  menus.map((m) => ({
    key: m.key,
    label: m.label,
    icon: () => {
      // v3.10：日志菜单套 NBadge 显示红点
      if (m.key === "logs") {
        return h(
          "div",
          { class: "nav-icon-wrapper", style: "position: relative; display: inline-flex;" },
          [
            h(SafeIcon, { component: m.icon, size: 20, class: "nav-icon" }),
            errorLog.unreadCount > 0
              ? h(NBadge, {
                  value: errorLog.unreadCount,
                  max: 99,
                  color: "#d03050",
                  class: "nav-error-badge",
                })
              : null,
          ],
        );
      }
      // 其他菜单：原样
      return h(SafeIcon, { component: m.icon, size: 20, class: "nav-icon" });
    },
  })),
);

/** 当前路由对应的菜单 key */
const activeKey = computed(() => {
  // 用菜单项 path 匹配当前 route.path 找到对应 key
  const matched = menus.find((m) => m.path === route.path);
  return matched?.key ?? "";
});

/** 菜单点击：跳转到对应路由 */
function onMenuSelect(key: string) {
  const target = menus.find((m) => m.key === key);
  if (target && route.path !== target.path) {
    void router.push(target.path);
  }
}
</script>

<template>
  <NLayout has-sider style="height: 100vh">
    <NLayoutSider
      bordered
      :width="64"
      :collapsed-width="64"
      collapse-mode="width"
      :collapsed="true"
      class="app-sider"
    >
      <NScrollbar>
        <NMenu
          :options="menuOptions"
          :value="activeKey"
          :collapsed="true"
          :collapsed-width="64"
          :indent="0"
          @update:value="onMenuSelect"
        />
      </NScrollbar>
    </NLayoutSider>
    <NLayout>
      <NLayoutHeader bordered style="height: 48px">
        <AppHeader />
      </NLayoutHeader>
      <NLayoutContent content-style="padding: 16px;">
        <RouterView />
      </NLayoutContent>
    </NLayout>
  </NLayout>
</template>

<style scoped>
.app-sider :deep(.nav-icon) {
  width: 20px;
  height: 20px;
  flex-shrink: 0;
  display: block;
  color: inherit;
  /* 兜底：万一 currentColor 在 WebView2 解析失败，至少有字面值 */
  stroke: #333;
}

/* v3.10：日志菜单红点定位（绝对定位在图标右上角） */
.app-sider :deep(.nav-icon-wrapper .nav-error-badge) {
  position: absolute;
  top: -4px;
  right: -8px;
  pointer-events: none;
}
</style>
