<script setup lang="ts">
/**
 * 主布局
 *
 * 详见 `PR/06-界面设计.md §1 整体布局`
 *
 * 结构：
 * - 左侧：8 项导航（窄边栏 64px，仅图标 + tooltip）
 * - 顶栏：AppHeader（项目名 + 状态指示 + 设置入口）
 * - 内容区：RouterView
 *
 * 导航项：
 * 1. 启动（首页）
 * 2. 核心版本
 * 3. 插件管理
 * 4. 模型路径
 * 5. 设置
 * 6. 日志
 * 7. 任务进度
 * 8. 关于
 */

import {
  NLayout,
  NLayoutSider,
  NLayoutHeader,
  NLayoutContent,
  NMenu,
  NScrollbar,
  NTooltip,
  type MenuOption,
} from "naive-ui";
import { h, computed } from "vue";
import { RouterLink, useRoute } from "vue-router";
import AppHeader from "@/components/AppHeader.vue";

const route = useRoute();

interface NavItem {
  key: string;
  label: string;
  icon: string;
  path: string;
}

const menus: readonly NavItem[] = [
  { key: "launch", label: "启动", icon: "🚀", path: "/launch" },
  { key: "core", label: "核心版本", icon: "🔄", path: "/core" },
  { key: "plugins", label: "插件管理", icon: "🧩", path: "/plugins" },
  { key: "models", label: "模型路径", icon: "📦", path: "/models" },
  { key: "settings", label: "设置", icon: "⚙️", path: "/settings" },
  { key: "logs", label: "日志", icon: "📜", path: "/logs" },
  { key: "tasks", label: "任务进度", icon: "📊", path: "/tasks" },
  { key: "about", label: "关于", icon: "ℹ️", path: "/about" },
] as const;

const menuOptions = computed<MenuOption[]>(() =>
  menus.map((m) => ({
    key: m.key,
    label: () =>
      h(
        NTooltip,
        { placement: "right" },
        {
          trigger: () =>
            h(
              RouterLink,
              { to: m.path, class: "nav-link" },
              {
                default: () => [
                  h("span", { class: "nav-icon" }, m.icon),
                  h("span", { class: "nav-label" }, m.label),
                ],
              },
            ),
          default: () => m.label,
        },
      ),
  })),
);

const activeKey = computed(() => (typeof route.name === "string" ? route.name : ""));
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
.app-sider :deep(.nav-link) {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 8px 0;
  text-decoration: none;
  color: inherit;
}

.app-sider :deep(.nav-icon) {
  font-size: 20px;
  line-height: 1;
}

.app-sider :deep(.nav-label) {
  font-size: 11px;
  margin-top: 2px;
  line-height: 1;
}
</style>
