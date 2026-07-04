/**
 * Config Store
 *
 * 设计模式：
 * - **Store (Flux)**：Pinia 集中管理 Config 状态
 * - **Observer**：监听后端 `config_changed` 事件自动同步
 *
 * 使用方式：
 * ```ts
 * const configStore = useConfigStore();
 * await configStore.load(); // 初始化加载
 * await configStore.update({ launch: { listen_port: 9000 } }); // 部分更新
 * ```
 *
 * 注意：组件不应直接修改 `config` ref，应通过 `update()` 方法
 * （会同步调用后端 + 自动 listen 同步）
 */

import { defineStore } from "pinia";
import { ref, computed } from "vue";
import { configGet, configUpdate, configReset } from "@/api/config";
import { listen, type UnlistenFn } from "@/api";
import type { Config, ConfigUpdate, LaunchMode, PreviewMethod } from "@/api/types";

export const useConfigStore = defineStore("config", () => {
  // ========== State ==========
  const config = ref<Config | null>(null);
  const loading = ref(false);
  const error = ref<string | null>(null);
  let unlistenFn: UnlistenFn | null = null;

  // ========== Getters ==========
  const isLoaded = computed(() => config.value !== null);
  const launchMode = computed<LaunchMode | null>(() => config.value?.launch.mode ?? null);
  const listenPort = computed<number | null>(() => config.value?.launch.listen_port ?? null);
  const comfyuiRoot = computed<string>(() => config.value?.paths.comfyui_root ?? "");
  const venvPath = computed<string>(() => config.value?.paths.venv_path ?? "");
  const previewMethod = computed<PreviewMethod | null>(
    () => config.value?.launch.preview_method ?? null,
  );

  // ========== Actions ==========

  /** 加载 Config（首次启动 / 手动刷新时调用） */
  async function load() {
    loading.value = true;
    error.value = null;
    try {
      config.value = await configGet();
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
      throw e;
    } finally {
      loading.value = false;
    }
  }

  /**
   * 部分更新 Config
   *
   * 流程：调用后端 → 后端 emit "config_changed" → 本 store 监听并更新 ref
   * （无需手动设置 config.value，监听器会自动同步）
   */
  async function update(update: ConfigUpdate) {
    const updated = await configUpdate(update);
    // 立即同步（不等事件，避免事件延迟造成 UI 短暂不一致）
    config.value = updated;
  }

  /** 重置 Config 到默认值（保留 paths） */
  async function reset() {
    const reset = await configReset();
    config.value = reset;
  }

  /**
   * 订阅后端 `config_changed` 事件
   *
   * 应在应用启动时（App.vue onMounted）调用一次。
   * 后端在 config_update / config_reset 后 emit 此事件。
   */
  async function subscribe() {
    if (unlistenFn) return; // 已订阅
    unlistenFn = await listen<Config>("config_changed", (event) => {
      config.value = event.payload;
    });
  }

  /** 取消订阅（应用卸载时调用） */
  function unsubscribe() {
    unlistenFn?.();
    unlistenFn = null;
  }

  return {
    // state
    config,
    loading,
    error,
    // getters
    isLoaded,
    launchMode,
    listenPort,
    comfyuiRoot,
    venvPath,
    previewMethod,
    // actions
    load,
    update,
    reset,
    subscribe,
    unsubscribe,
  };
});
