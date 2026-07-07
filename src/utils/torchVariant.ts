/**
 * TorchVariant 序列化 / 反序列化工具（v3.0 新增，F25）
 *
 * Config 中 `torch.torch_variant` 字段以 JSON 字符串形式存储
 * （避免循环依赖、便于迁移）。
 *
 * 后端 Rust 序列化形式：
 * - `{ vendor: "nvidia_cuda", version: "cu118" }`
 * - `{ vendor: "amd_rocm", version: "rocm6.3" }`
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
          obj.version === "cu126" ||
          obj.version === "cu128" ||
          obj.version === "cu130"
        ) {
          return { vendor: "nvidia_cuda", version: obj.version };
        }
        return null;
      case "amd_rocm":
        if (
          obj.version === "rocm6.3" ||
          obj.version === "rocm6.4" ||
          obj.version === "rocm7.0" ||
          obj.version === "rocm7.1" ||
          obj.version === "rocm7.2"
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
 * v3.10：解析 torch.__version__ 字符串 → TorchVariant
 *
 * 用途：Config.torch.torch_variant 未设置（首次启动 / 自动安装）时，
 * 从 envInfo.torch_version 字符串反向推断出变体，让设置页面单选列表能默认选中。
 *
 * 解析规则：
 * - "2.11.0+cu128" / "2.12.0+cu130" → `{ vendor: "nvidia_cuda", version: "cu128" }`
 * - "2.11.0+rocm6.4" → `{ vendor: "amd_rocm", version: "rocm6.4" }`
 * - "2.11.0+xpu" → `{ vendor: "intel_xpu" }`
 * - "2.4.0"（无后缀 = CPU） → `{ vendor: "cpu_only" }`
 * - 无法识别 → null
 *
 * 注意：这是降级路径，**不修改 Config**（避免污染用户配置）。
 * 仅用于 UI 展示，下一次用户手动切换时会写入 Config。
 */
export function parseTorchVersionString(version: string | null | undefined): TorchVariant | null {
  if (!version) return null;
  // NVIDIA CUDA：+cuXXX 后缀
  const cudaMatch = version.match(/\+cu(\d+)/);
  if (cudaMatch) {
    const num = parseInt(cudaMatch[1], 10);
    // 数字 → 字符串版本号（cu118 / cu126 / cu128 / cu130）
    if (num === 118) return { vendor: "nvidia_cuda", version: "cu118" };
    if (num === 126) return { vendor: "nvidia_cuda", version: "cu126" };
    if (num === 128) return { vendor: "nvidia_cuda", version: "cu128" };
    if (num === 130) return { vendor: "nvidia_cuda", version: "cu130" };
    // 未知 CUDA 版本：返回最接近的（向下兼容）
    if (num >= 130) return { vendor: "nvidia_cuda", version: "cu130" };
    if (num >= 128) return { vendor: "nvidia_cuda", version: "cu128" };
    if (num >= 126) return { vendor: "nvidia_cuda", version: "cu126" };
    if (num >= 118) return { vendor: "nvidia_cuda", version: "cu118" };
    return null;
  }
  // AMD ROCm：+rocmX.Y 后缀
  const rocmMatch = version.match(/\+rocm(\d+\.\d+)/);
  if (rocmMatch) {
    const ver = `rocm${rocmMatch[1]}`;
    if (
      ver === "rocm6.3" ||
      ver === "rocm6.4" ||
      ver === "rocm7.0" ||
      ver === "rocm7.1" ||
      ver === "rocm7.2"
    ) {
      return { vendor: "amd_rocm", version: ver as any };
    }
    return null;
  }
  // Intel XPU：+xpu 后缀
  if (version.includes("+xpu")) {
    return { vendor: "intel_xpu" };
  }
  // Apple MPS：torch 2.x base wheel 在 macOS 上是 MPS（无 + 后缀）
  // 这里通过 platform 区分，但 parseTorchVersionString 不接受 platform 参数
  // 所以只判断是否包含 mps 关键字
  if (version.toLowerCase().includes("mps")) {
    return { vendor: "apple_silicon" };
  }
  // 无后缀 = CPU
  // 例：2.4.0（纯版本号，无 + 后缀）→ CPU
  if (/^\d+\.\d+\.\d+$/.test(version.trim())) {
    return { vendor: "cpu_only" };
  }
  return null;
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
      if (version === "cu118" || version === "cu126" || version === "cu128" || version === "cu130") {
        return { vendor: "nvidia_cuda", version: version as any };
      }
      return null;
    case "amd_rocm":
      if (
        version === "rocm6.3" ||
        version === "rocm6.4" ||
        version === "rocm7.0" ||
        version === "rocm7.1" ||
        version === "rocm7.2"
      ) {
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

/** UI 显示名称
 *
 * v3.7：用硬编码映射替代原 replace 逻辑（原逻辑对 cu130 等会出错）
 */
export function variantLabel(variant: TorchVariant): string {
  switch (variant.vendor) {
    case "nvidia_cuda": {
      // 宽化为 string，避免 exhaustive switch 后 variant.version 被收窄为 never
      const ver: string = variant.version;
      switch (variant.version) {
        case "cu118": return "CUDA 11.8";
        case "cu126": return "CUDA 12.6";
        case "cu128": return "CUDA 12.8";
        case "cu130": return "CUDA 13.0";
      }
      return `CUDA ${ver}`;
    }
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
  // v3.7：对齐 PyTorch 2.11 官方 wheel
  const variants: TorchVariant[] = [
    // NVIDIA CUDA（最新在前，cu130 推荐）
    { vendor: "nvidia_cuda", version: "cu130" },
    { vendor: "nvidia_cuda", version: "cu128" },
    { vendor: "nvidia_cuda", version: "cu126" },
    { vendor: "nvidia_cuda", version: "cu118" },
    // AMD ROCm（最新在前）
    { vendor: "amd_rocm", version: "rocm7.2" },
    { vendor: "amd_rocm", version: "rocm7.1" },
    { vendor: "amd_rocm", version: "rocm7.0" },
    { vendor: "amd_rocm", version: "rocm6.4" },
    { vendor: "amd_rocm", version: "rocm6.3" },
    // 其他厂商
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
