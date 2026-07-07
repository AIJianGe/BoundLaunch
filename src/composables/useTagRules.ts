/**
 * v3.10：tag 规则前端版本（与后端 src-tauri/src/core_manager/tags.rs 保持一致）
 *
 * 用途：前端预估"将安装的默认版本"用于 UI 提示。
 * 不与后端权威结果冲突——后端 `update_latest_stable_for_installation` 是最终决策。
 *
 * 规则：
 * 1. 必须是稳定版（严格 vX.Y.Z）
 * 2. patch = 0 或 1
 * 3. **v3.10 新增**：跳过"首次大版本发布"（X.0.0 且 X 是最大主版本号）
 * 4. SemVer 倒序取最大
 *
 * 测试：见 src-tauri/src/core_manager/tags.rs 单元测试
 */

import type { TagInfo } from "@/api/types";

/** 判断 tag 是否为稳定版（前端版，与后端 STABLE_TAG_RE 一致） */
export function isStableTag(name: string): boolean {
    return /^v\d+\.\d+\.\d+$/.test(name);
}

/** 判断 patch 段是否为 0 或 1 */
export function isPatchZeroOrOne(name: string): boolean {
    const m = name.match(/^v(\d+)\.(\d+)\.(\d+)$/);
    if (!m) return false;
    const patch = parseInt(m[3], 10);
    return patch === 0 || patch === 1;
}

/** 判断 tag 是否为"首次大版本发布" */
export function isFirstMajorRelease(name: string, tags: TagInfo[]): boolean {
    const m = name.match(/^v(\d+)\.(\d+)\.(\d+)$/);
    if (!m) return false;
    const major = parseInt(m[1], 10);
    if (m[2] !== "0" || m[3] !== "0") return false;

    const maxMajor = tags
        .filter((t) => isStableTag(t.name))
        .reduce((acc, t) => {
            const tm = t.name.match(/^v(\d+)\./);
            if (!tm) return acc;
            const n = parseInt(tm[1], 10);
            return n > acc ? n : acc;
        }, -1);

    return major === maxMajor;
}

/**
 * 计算"引导安装默认版本"
 *
 * @param tags 全部 tag 列表（来自 coreListTagsClassified）
 * @returns 默认版本 tag name，找不到返回 null
 */
export function latestStableForInstallation(tags: TagInfo[]): string | null {
    const stable = tags.filter((t) => isStableTag(t.name) && t.is_stable);
    if (stable.length === 0) return null;

    // 过滤：patch=0/1 + 非首次大版本
    const filtered = stable.filter(
        (t) => isPatchZeroOrOne(t.name) && !isFirstMajorRelease(t.name, tags),
    );

    // SemVer 倒序取最大
    const sorted = filtered.sort((a, b) => compareTagDesc(a.name, b.name));
    if (sorted.length > 0) return sorted[0].name;

    // 兜底：原 latest_stable
    const allSorted = stable.sort((a, b) => compareTagDesc(a.name, b.name));
    return allSorted[0]?.name ?? null;
}

/**
 * SemVer 倒序比较（v3.3 / F33：修正字符串比较 bug）
 *
 * @returns a < b 返回正数（a 排在 b 后面）
 */
function compareTagDesc(a: string, b: string): number {
    const pa = a.match(/^v(\d+)\.(\d+)\.(\d+)$/);
    const pb = b.match(/^v(\d+)\.(\d+)\.(\d+)$/);
    if (!pa || !pb) return b.localeCompare(a);
    for (let i = 1; i <= 3; i++) {
        const na = parseInt(pa[i], 10);
        const nb = parseInt(pb[i], 10);
        if (na !== nb) return nb - na; // 倒序：a > b 返回负数
    }
    return 0;
}
