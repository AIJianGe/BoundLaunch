/**
 * TorchVariant 序列化 / 反序列化工具（v3.0 新增，F25）
 *
 * Config 中 `torch.torch_variant` 字段以 JSON 字符串形式存储
 * （避免循环依赖、便于迁移）。
 *
 * 后端 Rust 序列化形式：
 * - `{ vendor: "nvidia_cuda", version: "cu118" }`
 * - `{ vendor: "amd_rocm", version: "rocm6.0" }`
 * - `{ vendor: "intel_xpu" }`
 * - `{ vendor: "apple_silicon" }`
 * - `{ vendor: "cpu_only" }`
 *
 * 本工具函数：
 * - `parseTorchVariant(json)` → TorchVariant | null
 * - `serializeTorchVariant(variant)` → json string
 * - `variantLabel(variant)` → UI 显示文本
 * - `isVariantCompatible(variant, platform)` → 平台兼容性
 * - `compareVariants(a, b)` → 是否为同一变体
 */

import type { TorchVariant, TorchVariantOption, TorchVendor } from "@/api/types";

/**
 * 反序列化：从 JSON 字符串 → TorchVariant
 *
 * 容错：
 * - 解析失败 → null（前端显示"未配置"）
 * - 未知 vendor → null
 * - 缺失 version（仅对带 version 的变体）→ null
 */
export function parseTorchVariant(json: string | null | undefined): TorchVariant | null {
  if (!json) return null;
  try {
    const obj = JSON.parse(json) as { vendor?: string; version?: string };
    switch (obj.vendor) {
      case "nvidia_cuda":
        if (
          obj.version === "cu118" ||
          obj.version === "cu121" ||
          obj.version === "cu124"
        ) {
          return { vendor: "nvidia_cuda", version: obj.version };
        }
        return null;
      case "amd_rocm":
        if (
          obj.version === "rocm5.7" ||
          obj.version === "rocm6.0" ||
          obj.version === "rocm6.1"
        ) {
          return { vendor: "amd_rocm", version: obj.version };
        }
        return null;
      case "intel_xpu":
        return { vendor: "intel_xpu" };
      case "apple_silicon":
        return { vendor: "apple_silicon" };
      case "cpu_only":
        return { vendor: "cpu_only" };
      default:
        return null;
    }
  } catch {
    return null;
  }
}

/** 序列化：TorchVariant → JSON 字符串 */
export function serializeTorchVariant(variant: TorchVariant): string {
  return JSON.stringify(variant);
}

/**
 * TorchVariant → 短字符串 key（用于 UI RadioGroup v-model）
 *
 * 例：nvidia_cuda + cu121 → "nvidia_cuda:cu121"
 *     intel_xpu → "intel_xpu:"
 */
export function variantToKey(variant: TorchVariant): string {
  if ("version" in variant) {
    return `${variant.vendor}:${(variant as any).version}`;
  }
  return `${variant.vendor}:`;
}

/** 短字符串 key → TorchVariant（variantToKey 的逆操作） */
export function keyToVariant(key: string): TorchVariant | null {
  const [vendor, version] = key.split(":");
  switch (vendor) {
    case "nvidia_cuda":
      if (version === "cu118" || version === "cu121" || version === "cu124") {
        return { vendor: "nvidia_cuda", version: version as any };
      }
      return null;
    case "amd_rocm":
      if (version === "rocm5.7" || version === "rocm6.0" || version === "rocm6.1") {
        return { vendor: "amd_rocm", version: version as any };
      }
      return null;
    case "intel_xpu":
      return { vendor: "intel_xpu" };
    case "apple_silicon":
      return { vendor: "apple_silicon" };
    case "cpu_only":
      return { vendor: "cpu_only" };
    default:
      return null;
  }
}

/** UI 显示名称 */
export function variantLabel(variant: TorchVariant): string {
  switch (variant.vendor) {
    case "nvidia_cuda":
      // "cu121" → "CUDA 12.1"
      const v = variant.version;
      return `CUDA ${v.replace("cu", "").split("").join(".").replace(/^1\./, "1.")}`;
    case "amd_rocm":
      return `ROCm ${variant.version.replace("rocm", "")}`;
    case "intel_xpu":
      return "XPU";
    case "apple_silicon":
      return "MPS (CPU wheel)";
    case "cpu_only":
      return "CPU";
  }
}

/** 厂商名称（Tab 标题用） */
export function vendorLabel(vendor: TorchVendor): string {
  switch (vendor) {
    case "nvidia_cuda":
      return "NVIDIA";
    case "amd_rocm":
      return "AMD";
    case "intel_xpu":
      return "Intel";
    case "apple_silicon":
      return "Apple";
    case "cpu_only":
      return "CPU";
  }
}

/**
 * 平台兼容性检查（用于 UI Tab / 选项灰显）
 *
 * @param variant torch 变体
 * @param platform `"windows" | "linux" | "macos"`
 */
export function isVariantCompatible(
  variant: TorchVariant,
  platform: "windows" | "linux" | "macos",
): boolean {
  switch (variant.vendor) {
    case "amd_rocm":
      // AMD ROCm 官方主要支持 Linux；Windows 上有实验性 HIP SDK
      return platform === "linux" || platform === "windows";
    case "apple_silicon":
      // Apple Silicon 仅 macOS
      return platform === "macos";
    default:
      return true;
  }
}

/** 兼容性提示文案（不兼容时显示） */
export function variantIncompatibleHint(
  variant: TorchVariant,
  platform: "windows" | "linux" | "macos",
): string | undefined {
  if (isVariantCompatible(variant, platform)) return undefined;
  switch (variant.vendor) {
    case "amd_rocm":
      return "AMD ROCm 官方仅支持 Linux，Windows 仅有实验性 HIP SDK";
    case "apple_silicon":
      return "Apple Silicon (MPS) 仅支持 macOS";
    default:
      return undefined;
  }
}

/** 获取当前运行平台 */
export function currentPlatform(): "windows" | "linux" | "macos" {
  const ua = (typeof navigator !== "undefined" ? navigator.userAgent : "").toLowerCase();
  if (ua.includes("mac")) return "macos";
  if (ua.includes("linux")) return "linux";
  return "windows";
}

/** 比较两个 TorchVariant 是否相等（用于判断当前是否需要重装） */
export function compareVariants(a: TorchVariant | null, b: TorchVariant | null): boolean {
  if (a === null && b === null) return true;
  if (a === null || b === null) return false;
  if (a.vendor !== b.vendor) return false;
  if ("version" in a && "version" in b) {
    return (a as any).version === (b as any).version;
  }
  return true;
}

/**
 * 生成所有可选 torch 变体（用于 UI 一级 Tab + 二级选项渲染）
 *
 * @param platform 当前平台（用于兼容性检查）
 */
export function listAllTorchVariants(
  platform: "windows" | "linux" | "macos",
): TorchVariantOption[] {
  const variants: TorchVariant[] = [
    { vendor: "nvidia_cuda", version: "cu118" },
    { vendor: "nvidia_cuda", version: "cu121" },
    { vendor: "nvidia_cuda", version: "cu124" },
    { vendor: "amd_rocm", version: "rocm5.7" },
    { vendor: "amd_rocm", version: "rocm6.0" },
    { vendor: "amd_rocm", version: "rocm6.1" },
    { vendor: "intel_xpu" },
    { vendor: "apple_silicon" },
    { vendor: "cpu_only" },
  ];
  return variants.map((v) => ({
    variant: v,
    label: variantLabel(v),
    compatible: isVariantCompatible(v, platform),
    hint: variantIncompatibleHint(v, platform),
  }));
}

/** 按 vendor 分组（一级 Tab 渲染用） */
export function groupVariantsByVendor(
  platform: "windows" | "linux" | "macos",
): Record<TorchVendor, TorchVariantOption[]> {
  const all = listAllTorchVariants(platform);
  const groups: Record<TorchVendor, TorchVariantOption[]> = {
    nvidia_cuda: [],
    amd_rocm: [],
    intel_xpu: [],
    apple_silicon: [],
    cpu_only: [],
  };
  for (const opt of all) {
    groups[opt.variant.vendor].push(opt);
  }
  return groups;
}
