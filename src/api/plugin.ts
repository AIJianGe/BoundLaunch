import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
    ComfyUICoreRequirementsStatus,
    LocalRefInfo,
    PluginInfo,
    PluginListResult,
    PluginProgress,
    PluginProgressLog,
    PluginUpdateInfo,
    PreLaunchCheck,
    RemoteTagInfo,
    SwitchResult,
    UninstallResult,
    UpdateResult,
    VenvHealthReport,
    VenvImportWarning,
} from "./types";

/** 拉取插件列表（force=true 跳过 30s 缓存） */
export function pluginList(force = false): Promise<PluginListResult> {
    return invoke<PluginListResult>("plugin_list", { force });
}

/** 通过 git URL 安装插件到 custom_nodes（可选 checkout 到指定 tag） */
export function pluginInstall(
    url: string,
    tag?: string | null,
): Promise<PluginInfo> {
    return invoke<PluginInfo>("plugin_install", { url, tag: tag ?? null });
}

/** 列出远程仓库的 tag（不下载整个仓库，仅 ls-remote） */
export function pluginListRemoteTags(url: string): Promise<RemoteTagInfo[]> {
    return invoke<RemoteTagInfo[]>("plugin_list_remote_tags", { url });
}

/** 卸载插件（移动到 .trash 目录） */
export function pluginUninstall(name: string): Promise<UninstallResult> {
    return invoke<UninstallResult>("plugin_uninstall", { name });
}

/** 启用/禁用插件（切换 .disabled 后缀） */
export function pluginToggle(name: string, enabled: boolean): Promise<void> {
    return invoke<void>("plugin_toggle", { name, enabled });
}

/** 拉取单个插件更新（git pull） */
export function pluginUpdate(name: string): Promise<UpdateResult> {
    return invoke<UpdateResult>("plugin_update", { name });
}

/** 批量检查所有插件的远程更新状态 */
export function pluginCheckUpdates(): Promise<PluginUpdateInfo[]> {
    return invoke<PluginUpdateInfo[]>("plugin_check_updates");
}

/** 安装插件 requirements.txt 到 venv（可选 --force-reinstall） */
export function pluginInstallRequirements(
    name: string,
    forceReinstall = false,
): Promise<void> {
    return invoke<void>("plugin_install_requirements", {
        name,
        forceReinstall,
    });
}

/** v3.x：列出指定插件的可用 ref（本地 tag + branch） */
export function pluginListAvailableVersions(name: string): Promise<LocalRefInfo[]> {
    return invoke<LocalRefInfo[]>("plugin_list_available_versions", { name });
}

/** v3.x：切换插件到指定 ref（tag / branch / commit hash） */
export function pluginSwitchVersion(
    name: string,
    targetRef: string,
): Promise<SwitchResult> {
    return invoke<SwitchResult>("plugin_switch_version", {
        name,
        targetRef,
    });
}

/** v3.x：回滚到上次切版本前的 commit */
export function pluginRollbackVersion(name: string): Promise<PluginInfo> {
    return invoke<PluginInfo>("plugin_rollback_version", { name });
}

/** 监听后端 plugin_list_changed 事件 */
export function onPluginListChanged(cb: () => void) {
    return listen("plugin_list_changed", () => cb());
}

/** 监听后端 plugin_progress 事件（安装进度，0-100%） */
export function onPluginProgress(cb: (payload: PluginProgress) => void) {
    return listen<PluginProgress>("plugin_progress", (e) => cb(e.payload));
}

/** 监听后端 plugin_progress_log 事件（pip install 实时日志行） */
export function onPluginProgressLog(cb: (payload: PluginProgressLog) => void) {
    return listen<PluginProgressLog>("plugin_progress_log", (e) => cb(e.payload));
}

// ============== venv 健康检查（v3.x） ==============

/** venv 健康检查（检测损坏包 + 验证关键 import） */
export function pluginHealthCheckVenv(): Promise<VenvHealthReport> {
    return invoke<VenvHealthReport>("plugin_health_check_venv");
}

/** 一键修复 venv（清理损坏包 + 重新验证） */
export function pluginFixVenv(): Promise<VenvHealthReport> {
    return invoke<VenvHealthReport>("plugin_fix_venv");
}

// ============== v3.x：ComfyUI 核心依赖管理 ==============

/** 检查 ComfyUI 核心依赖状态（force=true 强制重装） */
export function pluginCheckComfyuiRequirements(
    forceReinstall = false,
): Promise<ComfyUICoreRequirementsStatus> {
    return invoke<ComfyUICoreRequirementsStatus>(
        "plugin_check_comfyui_requirements",
        { forceReinstall },
    );
}

/** 启动 ComfyUI 前的完整检查（core 依赖 + 待装插件） */
export function pluginLaunchPreCheck(
    forceReinstall = false,
): Promise<PreLaunchCheck> {
    return invoke<PreLaunchCheck>("plugin_launch_pre_check", {
        forceReinstall,
    });
}

/** 装 ComfyUI 核心依赖（force=true → --force-reinstall） */
export function pluginInstallComfyuiRequirements(
    forceReinstall = false,
): Promise<string> {
    return invoke<string>("plugin_install_comfyui_requirements", {
        forceReinstall,
    });
}

/**
 * 监听 venv 关键 import 失败事件
 * 触发时机：install_requirements 后 pip 退出 0 但 safetensors.torch 等模块找不到
 */
export function onVenvImportWarning(cb: (payload: VenvImportWarning) => void) {
    return listen<VenvImportWarning>("venv_import_warning", (e) => cb(e.payload));
}
