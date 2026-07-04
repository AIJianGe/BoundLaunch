/**
 * 命令预览构造器（前端镜像后端 `build_command`）
 *
 * 设计模式：
 * - **Strategy**：LaunchMode / PreviewMethod 不同枚举产出不同参数
 * - **Factory**：根据 Config 产出命令字符串
 *
 * 后端实现见 `src-tauri/src/process_launcher/command_builder.rs`。
 * 前端此函数仅用于 UI 预览，与后端实际执行命令保持一致。
 */

import type { Config, LaunchMode, PreviewMethod } from "@/api/types";

/** 将 LaunchMode 映射为 ComfyUI CLI 参数（镜像后端 `LaunchMode::to_args`） */
function launchModeArgs(mode: LaunchMode): string[] {
  switch (mode) {
    case "cpu":
      return ["--cpu", "--lowvram"];
    case "gpu_high":
      return ["--highvram"];
    case "gpu_low":
      return ["--lowvram"];
    case "gpu_no_vram":
      return ["--novram"];
    case "custom":
      return [];
  }
}

/** 将 PreviewMethod 映射为 ComfyUI CLI 参数（镜像后端 `PreviewMethod::to_arg`） */
function previewMethodArg(method: PreviewMethod): string {
  switch (method) {
    case "latent":
      return "latent";
    case "latent_upscale":
      return "latent-upscale";
    case "autoencoder":
      return "autoencoder";
    case "none":
      return "none";
  }
}

/** 简单的 shell 参数分割（用于 custom_args 预览，与后端 `sanitize_custom_args` 保持语义一致） */
function splitArgs(s: string): string[] {
  return s.trim().split(/\s+/).filter(Boolean);
}

/**
 * 根据当前 Config 构造命令预览字符串
 *
 * 输出格式：
 * ```
 * python main.py --highvram \
 *     --listen 127.0.0.1 --port 8188 \
 *     --preview-method latent --auto-launch
 * ```
 *
 * 第一行固定为 `python main.py <mode args>`，后续参数按 `\` 续行格式化。
 */
export function buildCommandPreview(config: Config): string {
  const launch = config.launch;
  const parts: string[] = ["python", "main.py"];

  // 显存策略
  parts.push(...launchModeArgs(launch.mode));

  // Custom 模式追加用户自定义参数
  if (launch.mode === "custom" && launch.custom_args.trim()) {
    parts.push(...splitArgs(launch.custom_args));
  }

  // 公共参数
  parts.push("--listen", launch.listen_host);
  parts.push("--port", String(launch.listen_port));
  parts.push("--preview-method", previewMethodArg(launch.preview_method));

  // 自动打开浏览器
  if (launch.auto_open_browser) {
    parts.push("--auto-launch");
  }

  // 高级参数
  const adv = launch.advanced;
  if (adv.use_split_cross_attention) parts.push("--use-split-cross-attention");
  if (adv.use_pytorch_cross_attention) parts.push("--use-pytorch-cross-attention");
  if (adv.force_fp32) parts.push("--force-fp32");
  if (adv.fp16_vae) parts.push("--fp16-vae");
  if (adv.bf16_vae) parts.push("--bf16-vae");
  if (adv.no_half) parts.push("--no-half");
  if (adv.no_half_vae) parts.push("--no-half-vae");
  if (adv.directml) parts.push("--directml");

  // 格式化：第一行 + 续行（每行最多 4 个 token，超出折行）
  return formatWithLineBreaks(parts);
}

/**
 * 将 token 数组格式化为多行命令字符串
 *
 * 规则：第一行包含 `python main.py` + 模式参数；后续参数每行最多 2 个键值对
 */
function formatWithLineBreaks(parts: string[]): string {
  if (parts.length <= 4) return parts.join(" ");

  // 找到模式参数之后的分割点
  // 第一段：python main.py <mode args> [<custom args>...]
  // 后续：--listen / --port / --preview-method / --auto-launch / 高级参数
  const firstLineEnd = findFirstLineBreak(parts);
  const firstLine = parts.slice(0, firstLineEnd).join(" ");
  const rest = parts.slice(firstLineEnd);

  if (rest.length === 0) return firstLine;

  // 后续参数按 2 个一组折行
  const lines: string[] = [`${firstLine} \\`];
  for (let i = 0; i < rest.length; i += 2) {
    const chunk = rest.slice(i, i + 2).join(" ");
    const isLast = i + 2 >= rest.length;
    lines.push(`    ${chunk}${isLast ? "" : " \\"}`);
  }
  return lines.join("\n");
}

/**
 * 找到第一行的分割点
 *
 * 算法：从 `--listen` 之前的位置断开
 */
function findFirstLineBreak(parts: string[]): number {
  const listenIdx = parts.indexOf("--listen");
  if (listenIdx > 0) return listenIdx;
  return Math.min(4, parts.length);
}
