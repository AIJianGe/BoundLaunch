/**
 * ModelPath Store
 *
 * 设计模式：
 * - **Store (Flux)**：集中管理模型路径配置与扫描结果
 * - **Observer**：监听 `model_scan_completed` 事件
 * - **Cache-Aside**：扫描结果缓存由后端管理（60s TTL）
 *
 * 使用方式：
 * ```ts
 * const store = useModelPathStore();
 * await store.scan(configStore.config.models.custom_root);
 * ```
 */

import { defineStore } from "pinia";
import { ref, computed } from "vue";
import {
  modelpathGenerate,
  modelpathRemove,
  modelpathScan,
  modelpathValidate,
} from "@/api/model";
import { listen, type UnlistenFn } from "@/api";
import type { ScanResult, SubdirInfo } from "@/api/types";

export const useModelPathStore = defineStore("modelPath", () => {
  // ========== State ==========
  const scanResult = ref<ScanResult | null>(null);
  const loading = ref(false);
  const error = ref<string | null>(null);
  const lastGenerated = ref<string | null>(null);

  const unlisteners: UnlistenFn[] = [];

  // ========== Getters ==========
  const isLoaded = computed(() => scanResult.value !== null);
  const subdirs = computed<SubdirInfo[]>(() => scanResult.value?.subdirs ?? []);
  const subdirCount = computed(() => subdirs.value.length);
  const totalFiles = computed(() =>
    subdirs.value.reduce((sum, s) => sum + s.file_count, 0),
  );

  // ========== Actions ==========

  /** 扫描根目录 */
  async function scan(root: string, force = false) {
    loading.value = true;
    error.value = null;
    try {
      await modelpathValidate(root);
      scanResult.value = await modelpathScan(root, force);
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
      throw e;
    } finally {
      loading.value = false;
    }
  }

  /** 生成 extra_model_paths.yaml */
  async function generate() {
    loading.value = true;
    try {
      const result = await modelpathGenerate();
      lastGenerated.value = result.generated_at;
      return result;
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
      throw e;
    } finally {
      loading.value = false;
    }
  }

  /** 删除 launcher 生成的 yaml */
  async function remove() {
    try {
      await modelpathRemove();
      lastGenerated.value = null;
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
      throw e;
    }
  }

  /** 仅校验路径 */
  async function validate(path: string) {
    try {
      await modelpathValidate(path);
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
      throw e;
    }
  }

  /** 订阅后端扫描完成事件 */
  async function subscribe() {
    if (unlisteners.length > 0) return;
    unlisteners.push(
      await listen<ScanResult>("model_scan_completed", (e) => {
        scanResult.value = e.payload;
      }),
    );
  }

  function unsubscribe() {
    unlisteners.forEach((un) => un());
    unlisteners.length = 0;
  }

  return {
    // state
    scanResult,
    loading,
    error,
    lastGenerated,
    // getters
    isLoaded,
    subdirs,
    subdirCount,
    totalFiles,
    // actions
    scan,
    generate,
    remove,
    validate,
    subscribe,
    unsubscribe,
  };
});
