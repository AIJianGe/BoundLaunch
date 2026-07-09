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
    const enabledCount = computed(() =>
        plugins.value.filter((p) => p.enabled).length,
    );
    const disabledCount = computed(() => totalCount.value - enabledCount.value);
    const hasUpdates = computed(() =>
        plugins.value.some((p) => p.has_updates === true),
    );

    // ========== Actions ==========

    /** 刷新插件列表（从后端获取，后端有 30s 缓存） */
    async function refresh(force = false) {
        loading.value = true;
        error.value = null;
        try {
            // 后端返回 PluginListResult { plugins, fetched_at }
            const result = await pluginList(force);
            plugins.value = result.plugins;
        } catch (e) {
            error.value = e instanceof Error ? e.message : String(e);
            throw e;
        } finally {
            loading.value = false;
        }
    }

    /** 通过 git URL 安装插件（可选 checkout 到指定 tag） */
    async function install(url: string, tag?: string | null) {
        loading.value = true;
        try {
            await pluginInstall(url, tag);
            // 后端会 emit plugin_list_changed，但保险起见也手动刷新
            await refresh(true);
        } finally {
            loading.value = false;
        }
    }

    /** 更新单个插件（git pull） */
    async function update(name: string) {
        try {
            await pluginUpdate(name);
            await refresh(true);
        } catch (e) {
            error.value = e instanceof Error ? e.message : String(e);
            throw e;
        }
    }

    /** 卸载插件（移到 .trash） */
    async function uninstall(name: string) {
        try {
            await pluginUninstall(name);
            await refresh(true);
        } catch (e) {
            error.value = e instanceof Error ? e.message : String(e);
            throw e;
        }
    }

    /** 启用/禁用插件（切换 .disabled 后缀） */
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

    /** 批量检查所有插件的远程更新状态，merge 回 plugins 列表 */
    async function checkUpdates() {
        loading.value = true;
        try {
            const updates = await pluginCheckUpdates();
            // 把更新检查结果 merge 回 plugins 列表
            // updates 是 PluginUpdateInfo[]，只包含 name + has_update + commits
            const updateMap = new Map(updates.map((u) => [u.name, u]));
            for (const p of plugins.value) {
                const u = updateMap.get(p.name);
                if (u) {
                    p.has_updates = u.has_update;
                }
            }
        } catch (e) {
            error.value = e instanceof Error ? e.message : String(e);
            throw e;
        } finally {
            loading.value = false;
        }
    }

    /** 订阅后端 `plugin_list_changed` 事件（install/uninstall/toggle 后 emit） */
    async function subscribe() {
        if (unlisteners.length > 0) return;
        unlisteners.push(
            await listen<void>("plugin_list_changed", () => {
                refresh(true).catch((e) =>
                    console.warn("[plugin] refresh on event failed:", e),
                );
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
