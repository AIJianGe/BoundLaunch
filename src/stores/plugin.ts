/**
 * Plugin Store
 *
 * 设计模式：
 * - **Store (Flux)**：集中管理插件列表
 * - **Observer**：监听 `plugin_list_changed` 事件自动刷新
 * - **Cache-Aside**：30s TTL 由后端管理，前端仅缓存最近一次结果
 *
 * 使用方式：
 * ```ts
 * const pluginStore = usePluginStore();
 * await pluginStore.subscribe();
 * await pluginStore.refresh();
 * ```
 */

import { defineStore } from "pinia";
import { ref, computed } from "vue";
import {
  pluginList,
  pluginInstall,
  pluginUpdate,
  pluginUninstall,
  pluginToggle,
  pluginInstallRequirements,
  pluginCheckUpdates,
} from "@/api/plugin";
import { listen, type UnlistenFn } from "@/api";
import type { PluginInfo } from "@/api/types";

export const usePluginStore = defineStore("plugin", () => {
  // ========== State ==========
  const plugins = ref<PluginInfo[]>([]);
  const loading = ref(false);
  const error = ref<string | null>(null);

  const unlisteners: UnlistenFn[] = [];

  // ========== Getters ==========
  const totalCount = computed(() => plugins.value.length);
  const enabledCount = computed(() => plugins.value.filter((p) => p.enabled).length);
  const disabledCount = computed(() => totalCount.value - enabledCount.value);
  const hasUpdates = computed(() => plugins.value.some((p) => p.has_updates));

  // ========== Actions ==========

  /** 刷新插件列表 */
  async function refresh() {
    loading.value = true;
    error.value = null;
    try {
      plugins.value = await pluginList();
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
      throw e;
    } finally {
      loading.value = false;
    }
  }

  /** 安装插件 */
  async function install(gitUrl: string) {
    loading.value = true;
    try {
      await pluginInstall(gitUrl);
      await refresh();
    } finally {
      loading.value = false;
    }
  }

  /** 更新单个插件 */
  async function update(name: string) {
    try {
      await pluginUpdate(name);
      await refresh();
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
      throw e;
    }
  }

  /** 卸载插件（移到 .trash） */
  async function uninstall(name: string) {
    try {
      await pluginUninstall(name);
      await refresh();
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
      throw e;
    }
  }

  /** 启用/禁用插件 */
  async function toggle(name: string, enabled: boolean) {
    try {
      await pluginToggle(name, enabled);
      // 乐观更新：立即同步本地状态
      const target = plugins.value.find((p) => p.name === name);
      if (target) {
        target.enabled = enabled;
      }
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
      throw e;
    }
  }

  /** 安装插件 requirements.txt */
  async function installRequirements(name: string) {
    try {
      await pluginInstallRequirements(name);
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
      throw e;
    }
  }

  /** 批量检查更新 */
  async function checkUpdates() {
    loading.value = true;
    try {
      plugins.value = await pluginCheckUpdates();
    } finally {
      loading.value = false;
    }
  }

  /** 订阅后端 `plugin_list_changed` 事件 */
  async function subscribe() {
    if (unlisteners.length > 0) return;
    unlisteners.push(
      await listen<void>("plugin_list_changed", () => {
        refresh().catch((e) => console.warn("plugin refresh:", e));
      }),
    );
  }

  function unsubscribe() {
    unlisteners.forEach((un) => un());
    unlisteners.length = 0;
  }

  return {
    // state
    plugins,
    loading,
    error,
    // getters
    totalCount,
    enabledCount,
    disabledCount,
    hasUpdates,
    // actions
    refresh,
    install,
    update,
    uninstall,
    toggle,
    installRequirements,
    checkUpdates,
    subscribe,
    unsubscribe,
  };
});
