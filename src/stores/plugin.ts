/**
 * Plugin Store
 *
 * 设计模式：
 * - **Store (Flux)**：集中管理插件列表
 * - **Observer**：监听 `plugin_list_changed` 事件自动刷新
 * - **Cache-Aside**：30s TTL 由后端管理，前端仅缓存最近一次结果
 *
 * v3.x 新增：
 * - 订阅 `plugin_progress` 事件，实时推送安装进度
 * - 订阅 `plugin_progress_log` 事件，接收 pip install 实时日志
 * - 维护 installProgress（当前任务状态）和 installLogs（实时日志缓冲）
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
    pluginListAvailableVersions,
    pluginSwitchVersion,
    pluginRollbackVersion,
    onPluginProgress,
    onPluginProgressLog,
    pluginHealthCheckVenv,
    pluginFixVenv,
    onVenvImportWarning,
    pluginCheckComfyuiRequirements,
    pluginLaunchPreCheck,
    pluginInstallComfyuiRequirements,
} from "@/api/plugin";
import { listen, type UnlistenFn } from "@/api";
import type {
    ComfyUICoreRequirementsStatus,
    LocalRefInfo,
    PluginInfo,
    PluginProgress,
    PluginProgressLog,
    PreLaunchCheck,
    SwitchResult,
    VenvHealthReport,
} from "@/api/types";

/** 单条安装日志行（前端内部表示） */
export interface InstallLogLine {
    level: "info" | "warn" | "error" | "debug" | "trace";
    message: string;
    timestamp: number;
}

/** 实时安装进度状态 */
export interface InstallProgress {
    active: boolean;
    plugin: string;
    /** "cloning" | "installing-deps" | "pulling" | "switching" | null */
    stage: "cloning" | "installing-deps" | "pulling" | "switching" | null;
    /** 0-100 */
    percent: number;
    /** 给用户看的提示文字 */
    message: string;
    /** 当前安装是否失败 */
    failed: boolean;
    /** 错误信息 */
    error: string;
    /** v3.x：上次切版本前的 commit（仅切换流程有值） */
    previousCommit?: string;
}

const INITIAL_PROGRESS: InstallProgress = {
    active: false,
    plugin: "",
    stage: null,
    percent: 0,
    message: "",
    failed: false,
    error: "",
};

const MAX_LOG_LINES = 200; // 环形缓冲 200 行

export const usePluginStore = defineStore("plugin", () => {
    // ========== State ==========
    const plugins = ref<PluginInfo[]>([]);
    const loading = ref(false);
    const error = ref<string | null>(null);

    // v3.x：实时安装进度 + 日志
    const installProgress = ref<InstallProgress>({ ...INITIAL_PROGRESS });
    const installLogs = ref<InstallLogLine[]>([]);

    // v3.x：venv 健康检查状态
    const venvHealth = ref<VenvHealthReport | null>(null);
    const venvHealthLoading = ref(false);
    const venvFixLoading = ref(false);

    // v3.x：v3.3 收到 import 失败事件时弹警告 toast（不阻塞主流程）
    const venvImportWarningMessage = ref<string | null>(null);

    // v3.x：ComfyUI 核心依赖状态
    const comfyuiCoreStatus = ref<ComfyUICoreRequirementsStatus | null>(null);
    const comfyuiCoreLoading = ref(false);
    const comfyuiCoreInstalling = ref(false);
    const lastPreCheck = ref<PreLaunchCheck | null>(null);

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

    // v3.x：venv 是否健康（null 表示未检测）
    const venvHealthy = computed(() => venvHealth.value?.status === "healthy");
    // v3.x：venv 是否有问题（broken/import failed/venv not found 等）
    const venvHasIssue = computed(() => {
        const s = venvHealth.value?.status;
        if (!s) return false;
        return s !== "healthy";
    });

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
        // 开启进度跟踪
        installProgress.value = {
            active: true,
            plugin: derivePluginNameFromUrl(url),
            stage: "cloning",
            percent: 0,
            message: `正在克隆 ${url}...`,
            failed: false,
            error: "",
        };
        installLogs.value = [];

        try {
            await pluginInstall(url, tag);
            // 后端会 emit plugin_list_changed + Done 事件，这里只是兜底
            await refresh(true);
        } catch (e) {
            installProgress.value.failed = true;
            installProgress.value.error = e instanceof Error ? e.message : String(e);
            throw e;
        } finally {
            // 注意：成功后 1.5s 后由 progress handler 自动 hide，
            // 失败时立刻 hide（让用户看到错误状态）
            if (installProgress.value.failed) {
                setTimeout(() => {
                    installProgress.value = { ...INITIAL_PROGRESS };
                }, 3000);
            }
        }
    }

    /** 更新单个插件（git pull） */
    async function update(name: string) {
        installProgress.value = {
            active: true,
            plugin: name,
            stage: "pulling",
            percent: 0,
            message: `正在拉取 ${name} 更新...`,
            failed: false,
            error: "",
        };
        installLogs.value = [];
        try {
            await pluginUpdate(name);
            await refresh(true);
        } catch (e) {
            installProgress.value.failed = true;
            installProgress.value.error = e instanceof Error ? e.message : String(e);
            throw e;
        } finally {
            if (installProgress.value.failed) {
                setTimeout(() => {
                    installProgress.value = { ...INITIAL_PROGRESS };
                }, 3000);
            }
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

    /** 安装插件 requirements.txt（force_reinstall 用于切版本场景） */
    async function installRequirements(name: string, forceReinstall = false) {
        installProgress.value = {
            active: true,
            plugin: name,
            stage: "installing-deps",
            percent: 0,
            message: forceReinstall
                ? `正在重装 ${name} 依赖...`
                : `正在装 ${name} 依赖...`,
            failed: false,
            error: "",
        };
        installLogs.value = [];
        try {
            await pluginInstallRequirements(name, forceReinstall);
            await refresh(true);
        } catch (e) {
            installProgress.value.failed = true;
            installProgress.value.error = e instanceof Error ? e.message : String(e);
            throw e;
        } finally {
            if (installProgress.value.failed) {
                setTimeout(() => {
                    installProgress.value = { ...INITIAL_PROGRESS };
                }, 3000);
            }
        }
    }

    // ============ v3.x：版本切换 ============

    /** 列出插件可用 ref（用于切版本弹窗） */
    async function listAvailableVersions(name: string): Promise<LocalRefInfo[]> {
        return await pluginListAvailableVersions(name);
    }

    /** 切换插件到指定 ref（tag / branch / commit） */
    async function switchVersion(
        name: string,
        targetRef: string,
    ): Promise<SwitchResult> {
        installProgress.value = {
            active: true,
            plugin: name,
            stage: "switching",
            percent: 0,
            message: `正在切到 ${targetRef}...`,
            failed: false,
            error: "",
        };
        installLogs.value = [];
        try {
            const result = await pluginSwitchVersion(name, targetRef);
            installProgress.value.previousCommit = result.previous_commit;
            installProgress.value.message = result.need_restart
                ? `已切到 ${targetRef}（ComfyUI 需重启才能生效）`
                : `已切到 ${targetRef}`;
            await refresh(true);
            return result;
        } catch (e) {
            installProgress.value.failed = true;
            installProgress.value.error = e instanceof Error ? e.message : String(e);
            throw e;
        } finally {
            if (installProgress.value.failed) {
                setTimeout(() => {
                    installProgress.value = { ...INITIAL_PROGRESS };
                }, 3000);
            }
        }
    }

    /** 回滚到上次切版本前的 commit */
    async function rollbackVersion(name: string) {
        installProgress.value = {
            active: true,
            plugin: name,
            stage: "switching",
            percent: 0,
            message: `正在回滚 ${name}...`,
            failed: false,
            error: "",
        };
        try {
            await pluginRollbackVersion(name);
            await refresh(true);
            installProgress.value.message = `${name} 已回滚`;
        } catch (e) {
            installProgress.value.failed = true;
            installProgress.value.error = e instanceof Error ? e.message : String(e);
            throw e;
        } finally {
            if (installProgress.value.failed) {
                setTimeout(() => {
                    installProgress.value = { ...INITIAL_PROGRESS };
                }, 3000);
            }
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

    /** 处理 plugin_progress 事件 */
    function handleProgress(p: PluginProgress) {
        switch (p.stage) {
            case "cloning":
                installProgress.value.stage = "cloning";
                installProgress.value.percent = p.percent;
                installProgress.value.message = "正在克隆仓库...";
                break;
            case "installing_requirements":
                installProgress.value.stage = "installing-deps";
                installProgress.value.percent = p.percent;
                installProgress.value.message = "正在装依赖...";
                break;
            case "requirements_percent":
                // 中间 percent 推送（保持当前 stage）
                installProgress.value.percent = p.percent;
                installProgress.value.message = `正在装依赖... ${p.percent}%`;
                break;
            case "pulling":
                installProgress.value.stage = "pulling";
                installProgress.value.percent = p.percent;
                installProgress.value.message = "正在拉取更新...";
                break;
            case "done":
                installProgress.value.percent = 100;
                installProgress.value.message = "安装完成";
                // 1.5s 后自动隐藏
                setTimeout(() => {
                    if (
                        installProgress.value.active &&
                        !installProgress.value.failed
                    ) {
                        installProgress.value = { ...INITIAL_PROGRESS };
                    }
                }, 1500);
                break;
            case "failed":
                installProgress.value.failed = true;
                installProgress.value.error = p.error;
                installProgress.value.message = `失败：${p.error}`;
                break;
        }
    }

    /** 处理 plugin_progress_log 事件（追加到 installLogs） */
    function handleProgressLog(line: PluginProgressLog) {
        installLogs.value.push({
            level: line.level,
            message: line.message,
            timestamp: Date.now(),
        });
        // 环形缓冲
        if (installLogs.value.length > MAX_LOG_LINES) {
            installLogs.value.splice(0, installLogs.value.length - MAX_LOG_LINES);
        }
    }

    /** 清空日志（折叠/展开切换时不丢失，仅在用户点"清空"按钮时调） */
    function clearLogs() {
        installLogs.value = [];
    }

    // ============ v3.x：venv 健康检查 ============

    /** 跑 venv 健康检查（典型调用：打开插件页时 + 点"修复"按钮时） */
    async function healthCheckVenv(): Promise<VenvHealthReport> {
        venvHealthLoading.value = true;
        try {
            const report = await pluginHealthCheckVenv();
            venvHealth.value = report;
            return report;
        } catch (e) {
            error.value = e instanceof Error ? e.message : String(e);
            throw e;
        } finally {
            venvHealthLoading.value = false;
        }
    }

    /** 一键修复 venv（清理损坏包 + 重新验证） */
    async function fixVenv(): Promise<VenvHealthReport> {
        venvFixLoading.value = true;
        try {
            const report = await pluginFixVenv();
            venvHealth.value = report;
            return report;
        } catch (e) {
            error.value = e instanceof Error ? e.message : String(e);
            throw e;
        } finally {
            venvFixLoading.value = false;
        }
    }

    // ========== v3.x：ComfyUI 核心依赖管理 ==========

    /**
     * 检查 ComfyUI 核心依赖状态（hash 跟踪）
     *
     * @param forceReinstall 是否强制重装
     * @returns 当前状态（needs_install + reason）
     */
    async function checkComfyuiCore(forceReinstall = false): Promise<ComfyUICoreRequirementsStatus> {
        comfyuiCoreLoading.value = true;
        try {
            const status = await pluginCheckComfyuiRequirements(forceReinstall);
            comfyuiCoreStatus.value = status;
            return status;
        } catch (e) {
            error.value = e instanceof Error ? e.message : String(e);
            throw e;
        } finally {
            comfyuiCoreLoading.value = false;
        }
    }

    /**
     * 启动 ComfyUI 前的完整检查（核心依赖 + 待装插件）
     *
     * @param forceReinstall 是否强制重装
     * @returns 完整报告（含 core_requirements + plugins_needing_install + all_ok）
     */
    async function preCheckLaunch(forceReinstall = false): Promise<PreLaunchCheck> {
        comfyuiCoreLoading.value = true;
        try {
            const check = await pluginLaunchPreCheck(forceReinstall);
            lastPreCheck.value = check;
            // 同步刷新核心状态到 store
            comfyuiCoreStatus.value = check.core_requirements;
            return check;
        } catch (e) {
            error.value = e instanceof Error ? e.message : String(e);
            throw e;
        } finally {
            comfyuiCoreLoading.value = false;
        }
    }

    /**
     * 装 ComfyUI 核心依赖（自动复用 plugin 进度 + 日志面板）
     *
     * @param forceReinstall 是否加 --force-reinstall 参数
     * @returns 装成功时的 hash
     */
    async function installComfyuiCore(forceReinstall = false): Promise<string> {
        comfyuiCoreInstalling.value = true;
        try {
            // 进度面板准备：复用 plugin 的 installProgress 机制
            installProgress.value = {
                active: true,
                plugin: "ComfyUI 核心",
                stage: "installing-deps",
                percent: 0,
                message: "",
                failed: false,
                error: "",
            };
            const hash = await pluginInstallComfyuiRequirements(forceReinstall);
            // 装完刷新状态
            await checkComfyuiCore(false);
            return hash;
        } catch (e) {
            error.value = e instanceof Error ? e.message : String(e);
            throw e;
        } finally {
            comfyuiCoreInstalling.value = false;
        }
    }

    /** ComfyUI 核心依赖是否需要重装（基于最近一次检查结果） */
    const comfyuiCoreNeedsInstall = computed(() => {
        return comfyuiCoreStatus.value?.needs_install ?? false;
    });

    /** 清空 venv 警告（toast 关闭按钮） */
    function clearVenvImportWarning() {
        venvImportWarningMessage.value = null;
    }

    /** 订阅后端事件（list_changed / progress / progress_log / venv_import_warning） */
    async function subscribe() {
        if (unlisteners.length > 0) return;
        unlisteners.push(
            await listen<void>("plugin_list_changed", () => {
                refresh(true).catch((e) =>
                    console.warn("[plugin] refresh on event failed:", e),
                );
            }),
            await onPluginProgress((p) => handleProgress(p)),
            await onPluginProgressLog((line) => handleProgressLog(line)),
            await onVenvImportWarning((w) => {
                venvImportWarningMessage.value = `插件 ${w.plugin} 装依赖时检测到 venv 异常：${w.summary}`;
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
        installProgress,
        installLogs,
        // v3.x：venv 健康
        venvHealth,
        venvHealthLoading,
        venvFixLoading,
        venvImportWarningMessage,
        // getters
        totalCount,
        enabledCount,
        disabledCount,
        hasUpdates,
        venvHealthy,
        venvHasIssue,
        // v3.x：ComfyUI 核心依赖
        comfyuiCoreStatus,
        comfyuiCoreLoading,
        comfyuiCoreInstalling,
        comfyuiCoreNeedsInstall,
        lastPreCheck,
        // actions
        refresh,
        install,
        update,
        uninstall,
        toggle,
        installRequirements,
        checkUpdates,
        clearLogs,
        // v3.x
        listAvailableVersions,
        switchVersion,
        rollbackVersion,
        healthCheckVenv,
        fixVenv,
        clearVenvImportWarning,
        checkComfyuiCore,
        preCheckLaunch,
        installComfyuiCore,
        subscribe,
        unsubscribe,
    };
});

/** 从 git URL 推插件名（与后端 derive_plugin_name 行为对齐） */
function derivePluginNameFromUrl(url: string): string {
    const trimmed = url.replace(/\.git$/, "").split("/").filter(Boolean);
    return trimmed[trimmed.length - 1] ?? "";
}
