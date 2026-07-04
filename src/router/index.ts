import { createRouter, createWebHistory, type RouteRecordRaw } from "vue-router";
import { setupRouterGuards } from "./guards";

const routes: RouteRecordRaw[] = [
  {
    path: "/onboarding",
    name: "onboarding",
    component: () => import("@/views/OnboardingPage.vue"),
    meta: { title: "首次运行向导" },
  },
  {
    path: "/",
    component: () => import("@/layouts/MainLayout.vue"),
    children: [
      { path: "", redirect: "/launch" },
      { path: "launch", name: "launch", component: () => import("@/views/LaunchPage.vue"), meta: { title: "启动" } },
      { path: "core", name: "core", component: () => import("@/views/CoreVersionPage.vue"), meta: { title: "核心版本" } },
      { path: "plugins", name: "plugins", component: () => import("@/views/PluginPage.vue"), meta: { title: "插件管理" } },
      { path: "models", name: "models", component: () => import("@/views/ModelPathPage.vue"), meta: { title: "模型路径" } },
      { path: "settings", name: "settings", component: () => import("@/views/SettingsPage.vue"), meta: { title: "设置" } },
      { path: "logs", name: "logs", component: () => import("@/views/LogsPage.vue"), meta: { title: "日志" } },
      { path: "tasks", name: "tasks", component: () => import("@/views/TaskCenterPage.vue"), meta: { title: "任务进度" } },
      { path: "about", name: "about", component: () => import("@/views/AboutPage.vue"), meta: { title: "关于" } },
    ],
  },
  {
    path: "/error",
    name: "error",
    component: () => import("@/components/ErrorPage.vue"),
    meta: { title: "出错了" },
  },
  {
    path: "/:pathMatch(.*)*",
    redirect: "/launch",
  },
];

const router = createRouter({
  history: createWebHistory(),
  routes,
});

// 路由守卫：未配置 config 强制跳 onboarding
setupRouterGuards(router);

// 标题同步
router.afterEach((to) => {
  document.title = to.meta.title ? `${to.meta.title} - 无界启动器` : "无界启动器";
});

export default router;
