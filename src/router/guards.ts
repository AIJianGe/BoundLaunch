/**
 * 路由守卫
 *
 * 详见 `PR/06-界面设计.md §0 首次运行向导`
 *
 * 规则：
 * - 未配置 config.toml（comfyui_root 为空）→ 强制跳 /onboarding
 * - 已配置 → 跳过 /onboarding 直接到主界面
 *
 * 触发时机：每次路由切换前执行
 *
 * 注意：守卫依赖 ConfigStore，必须在 App.vue 加载 Config 之后才能正确判断
 */

import type { Router } from "vue-router";
import { useConfigStore } from "@/stores/config";

const ONBOARDING_ROUTE = "/onboarding";
const DEFAULT_REDIRECT = "/launch";

/**
 * 安装路由守卫
 *
 * 应在 main.ts 中创建 router 后调用：
 * ```ts
 * import { setupRouterGuards } from "@/router/guards";
 * setupRouterGuards(router);
 * ```
 */
export function setupRouterGuards(router: Router) {
  router.beforeEach((to) => {
    const configStore = useConfigStore();

    // Config 未加载完成时放行（首次访问根路径会被 App.vue 的 load() 拦截）
    if (!configStore.isLoaded) {
      return true;
    }

    const isOnboarding = to.path === ONBOARDING_ROUTE;
    const needsOnboarding = !configStore.comfyuiRoot;

    if (needsOnboarding && !isOnboarding) {
      // 需要初始化 → 强制跳 onboarding
      return { path: ONBOARDING_ROUTE, replace: true };
    }

    if (!needsOnboarding && isOnboarding) {
      // 已初始化 → 跳过 onboarding 直接到主界面
      return { path: DEFAULT_REDIRECT, replace: true };
    }

    return true;
  });
}
