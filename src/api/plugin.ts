import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
    PluginInfo,
    PluginListResult,
    PluginUpdateInfo,
    RemoteTagInfo,
    UninstallResult,
    UpdateResult,
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

/** 安装插件 requirements.txt 到 venv */
export function pluginInstallRequirements(name: string): Promise<void> {
    return invoke<void>("plugin_install_requirements", { name });
}

/** 监听后端 plugin_list_changed 事件 */
export function onPluginListChanged(cb: () => void) {
    return listen("plugin_list_changed", () => cb());
}

/** 监听后端 plugin_progress 事件（安装/更新进度） */
export function onPluginProgress(
    cb: (payload: { plugin: string; stage: string; message: string }) => void,
) {
    return listen<{
        plugin: string;
        stage: string;
        message: string;
    }>("plugin_progress", (e) => cb(e.payload));
}
