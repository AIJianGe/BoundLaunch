/**
 * v3.10：tag 规则前端版本（与后端 src-tauri/src/core_manager/tags.rs 保持一致）
 *
 * 用途：前端预估"将安装的默认版本"用于 UI 提示。
 * 不与后端权威结果冲突——后端 `update_latest_stable_for_installation` 是最终决策。
 *
 * 规则：
 * 1. 必须是稳定版（严格 vX.Y.Z）
 * 2. patch = 0 或 1
 * 3. **v3.11.8 方案 B 兜底**：若 tags 中存在 `v0.27.0`，直接返回它
 *    - 用户明确要求"引导安装固定死安装 v0.27.0"
 * 4. **v3.11.8 方案 A**：过滤掉 major==0 && minor==0 的远古占位 tag（v0.0.x）
 *    - 防止 v0.0.1 等 patch=1 的远古 tag 在 date 异常时被误选
 * 5. **v3.11.6 关键修复**：按 tag date 倒序选择（不再用 SemVer 比较）
 *    - 原因：ComfyUI tag 历史非单调（v1.0.1 是 2017 年老 tag，v0.27.0 是 2025 年新版）
 *    - SemVer 比较会错误选 v1.0.1（major 1 > major 0）
 *    - 需求文档本来就写"发布日期最后"，现在实现与文档一致
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

/**
 * v3.11.8 方案 A：判断 tag 是否为"远古占位 tag"（major==0 && minor==0，即 v0.0.x）
 *
 * 例：
 * - `v0.0.1` → true
 * - `v0.27.0` → false（minor=27 ≠ 0）
 * - `v1.0.0` → false（major=1 ≠ 0）
 */
export function isLegacyPlaceholderTag(name: string): boolean {
    const m = name.match(/^v(\d+)\.(\d+)\.(\d+)$/);
    if (!m) return false;
    const major = parseInt(m[1], 10);
    const minor = parseInt(m[2], 10);
    return major === 0 && minor === 0;
}

/**
 * 计算"引导安装默认版本"
 *
 * @param tags 全部 tag 列表（来自 coreListTagsClassified）
 * @returns 默认版本 tag name，找不到返回 null
 */
export function latestStableForInstallation(tags: TagInfo[]): string | null {
    // 方案 B：v0.27.0 绝对优先（与后端 tags.rs 保持一致）
    const hardcoded = tags.find((t) => t.name === "v0.27.0");
    if (hardcoded) {
        return hardcoded.name;
    }

    const stable = tags.filter((t) => isStableTag(t.name) && t.is_stable);
    if (stable.length === 0) return null;

    // 过滤：patch=0/1 + 非远古占位 tag（方案 A）
    const filtered = stable.filter(
        (t) => isPatchZeroOrOne(t.name) && !isLegacyPlaceholderTag(t.name),
    );

    // v3.11.6: 按 tag date 倒序取最新（不再用 SemVer 比较）
    const sorted = filtered.sort((a, b) => {
        const da = a.date ? new Date(a.date).getTime() : 0;
        const db = b.date ? new Date(b.date).getTime() : 0;
        return db - da; // 倒序：新的排前面
    });
    if (sorted.length > 0) return sorted[0].name;

    // 兜底：原 latest_stable（SemVer 倒序）
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
