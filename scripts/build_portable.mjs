#!/usr/bin/env node
/**
 * BoundLaunch 绿色版（Portable）打包脚本
 *
 * ## 目标
 *
 * 把 BoundLaunch.exe 打包成绿色版：
 * - 单个 zip，解压即用
 * - 用户可以复制整个文件夹得到独立环境
 * - 不依赖系统安装、不写注册表、不写 %APPDATA%
 *
 * ## 流程
 *
 * 1. 调用 `cargo build --release`（如果产物已存在则跳过）
 * 2. 调用 `npx tauri build --target app`（产生 unpacked 目录）
 *    或直接复制 `target/release/BoundLaunch.exe + .dll`
 * 3. 准备 portable 目录：
 *    ```
 *    BoundLaunch-portable-v0.1.0/
 *    ├── BoundLaunch.exe
 *    ├── BoundLaunch.dll
 *    ├── resources/uv/*
 *    ├── launcher-portable.dat
 *    ├── README.md
 *    ├── .gitignore
 *    └── .boundlaunch/    (空目录，首次启动时填充)
 *    ```
 * 4. 打包成 zip：`dist/BoundLaunch-portable-v0.1.0.zip`
 *
 * ## 用法
 *
 * ```bash
 * node scripts/build_portable.mjs                # 默认版本号（package.json）
 * node scripts/build_portable.mjs --skip-build   # 跳过 cargo build（已构建时）
 * ```
 *
 * @author BoundLaunch Team
 * @version 1.0.0
 */

import { spawnSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  copyFileSync,
  copySync,
  readFileSync,
  writeFileSync,
  readdirSync,
  statSync,
  rmSync,
} from "node:fs";
import { join, resolve, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { createWriteStream } from "node:fs";
import { createGzip } from "node:zlib";
import { createReadStream } from "node:fs";

// ============================================================================
// 配置
// ============================================================================

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const PROJECT_ROOT = resolve(__dirname, "..");
const PACKAGE_JSON = join(PROJECT_ROOT, "package.json");
const TAURI_TARGET = join(PROJECT_ROOT, "src-tauri", "target", "release");
const DIST_DIR = join(PROJECT_ROOT, "dist");
const RESOURCES_UV = join(PROJECT_ROOT, "src-tauri", "resources", "uv");

const SKIP_BUILD = process.argv.includes("--skip-build");

// ============================================================================
// 工具函数
// ============================================================================

function log(level, msg) {
  const colors = {
    info: "\x1b[36m",
    warn: "\x1b[33m",
    error: "\x1b[31m",
    success: "\x1b[32m",
  };
  const reset = "\x1b[0m";
  const color = colors[level] || "";
  console.log(`${color}[${level.toUpperCase()}]${reset} ${msg}`);
}

function getVersion() {
  const pkg = JSON.parse(readFileSync(PACKAGE_JSON, "utf-8"));
  return pkg.version || "0.1.0";
}

function formatSize(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(2)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(2)} MB`;
}

// ============================================================================
// 阶段 1: cargo build --release
// ============================================================================

function buildRust() {
  log("info", "===== 阶段 1/5: 编译 Rust (cargo build --release) =====");
  const result = spawnSync("cargo", ["build", "--release", "--manifest-path", "src-tauri/Cargo.toml"], {
    cwd: PROJECT_ROOT,
    stdio: "inherit",
  });
  if (result.status !== 0) {
    log("error", "cargo build 失败");
    process.exit(1);
  }
  log("success", "Rust 编译完成");
}

// ============================================================================
// 阶段 2: 准备 portable 目录
// ============================================================================

function preparePortableDir(version) {
  log("info", "===== 阶段 2/5: 准备 portable 目录 =====");
  const portableDirName = `BoundLaunch-v${version}`;
  const portableDir = join(DIST_DIR, portableDirName);

  // 清理旧产物
  if (existsSync(portableDir)) {
    rmSync(portableDir, { recursive: true, force: true });
  }
  mkdirSync(portableDir, { recursive: true });

  // 拷贝 BoundLaunch.exe
  const exeName = "BoundLaunch.exe";
  const exeSrc = join(TAURI_TARGET, exeName);
  if (!existsSync(exeSrc)) {
    log("error", `未找到 ${exeSrc}`);
    log("error", "请先运行 `cargo build --release`");
    process.exit(1);
  }
  copyFileSync(exeSrc, join(portableDir, exeName));
  log("info", `已拷贝 ${exeName} (${formatSize(statSync(exeSrc).size)})`);

  // 拷贝 BoundLaunch.dll
  const dllName = "BoundLaunch.dll";
  const dllSrc = join(TAURI_TARGET, dllName);
  if (existsSync(dllSrc)) {
    copyFileSync(dllSrc, join(portableDir, dllName));
    log("info", `已拷贝 ${dllName} (${formatSize(statSync(dllSrc).size)})`);
  } else {
    log("warn", `未找到 ${dllSrc}（跳过）`);
  }

  // 拷贝 resources/uv
  if (existsSync(RESOURCES_UV)) {
    const uvTarget = join(portableDir, "resources", "uv");
    copySync(RESOURCES_UV, uvTarget, { recursive: true });
    const uvFiles = readdirSync(uvTarget);
    log("info", `已拷贝 resources/uv/ (${uvFiles.length} 个文件)`);
  } else {
    log("warn", `未找到 ${RESOURCES_UV}（跳过）`);
  }

  // 写 launcher-portable.dat
  const portableDat = `# BoundLaunch 绿色版环境配置（v3.x）
# 首次启动时会自动生成。手动编辑后重启启动器生效。

version = 1

# 环境名（用于日志、UI 显示、数据库命名空间）
# 默认用目录名兜底，可改成"SD15工作流"等有意义的名称
name = ""

# ComfyUI 启动端口（多环境时改这个避免冲突）
port = 8188

# custom_nodes 默认在 <ComfyUI>/custom_nodes/ 内
# 如需放到 ComfyUI 外（如 D:/SharedCustomNodes）：
# 1. 改下面 custom_nodes 字段
# 2. 启动器会自动给 ComfyUI 加 --base-directory 参数
# 3. ComfyUI 的 folder_paths.py 会从指定 base 找 custom_nodes / models / input / output
#
# custom_nodes = "D:/SharedCustomNodes"

[paths]
# ComfyUI 核心目录（相对当前目录）
comfyui = "ComfyUI"

# Python 虚拟环境
venv = "data/venv"

# custom_nodes 目录（默认在 ComfyUI 内）
custom_nodes = "ComfyUI/custom_nodes"

# 模型目录
models = "ComfyUI/models"

# launcher 私有数据（数据库、日志、配置）
boundlaunch_data = ".boundlaunch"
`;
  writeFileSync(join(portableDir, "launcher-portable.dat"), portableDat);
  log("info", "已写 launcher-portable.dat");

  // 写 README.md
  const readme = `# BoundLaunch 绿色版 v${version}

## 快速开始

1. 双击 \`BoundLaunch.exe\` 启动
2. 首次启动会检测到没有 ComfyUI，按提示装
3. 装好后自动启动 ComfyUI

## 目录结构

\`\`\`
BoundLaunch-v${version}/
├── BoundLaunch.exe          ← 启动器
├── BoundLaunch.dll          ← 启动器库
├── resources/uv/            ← 启动器资源
├── launcher-portable.dat    ← 环境配置
├── ComfyUI/                 ← ComfyUI 核心（装好后有）
├── data/                    ← Python 环境 + custom_nodes
│   ├── venv/                ← Python venv（含 torch 4.2 GB）
│   ├── custom_nodes/        ← 装的插件
│   └── models/              ← 模型
└── .boundlaunch/            ← launcher 私有数据
    ├── launcher.db          ← SQLite
    └── logs/                ← 日志
\`\`\`

## 创建新环境

复制整个文件夹到新位置即可：

\`\`\`powershell
# 创建环境 B（基于环境 A 完整复制）
Copy-Item -Recurse "D:\\BoundEnvA" "D:\\BoundEnvB"

# （可选）改 B 的名字和端口避免冲突
notepad D:\\BoundEnvB\\launcher-portable.dat
# 把 name 改成 "EnvB"、port 改成 8189
\`\`\`

之后双击 \`D:\\BoundEnvB\\BoundLaunch.exe\` 即可启动新环境。

## 共享模型目录（可选）

如果不想复制大模型，编辑 \`launcher-portable.dat\`：

\`\`\`toml
[paths]
models = "D:/SharedModels"  # 改成共享目录
\`\`\`

## 共享 custom_nodes（可选）

如果想多个环境共享同一份 custom_nodes，编辑 \`launcher-portable.dat\`：

\`\`\`toml
[paths]
custom_nodes = "D:/SharedCustomNodes"
\`\`\`

启动器会自动给 ComfyUI 加 \`--base-directory\` 参数。

## 升级启动器

\`\`\`powershell
# 手动：下载新版覆盖 BoundLaunch.exe + BoundLaunch.dll
# 批量：写一个 PS 脚本遍历所有环境目录更新
\`\`\`

## 常见问题

### Q: 启动器报 "WebView2 not found"
A: 安装 [WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/) 后重试

### Q: 端口冲突
A: 编辑 \`launcher-portable.dat\` 的 \`port\` 字段改成空闲端口

### Q: 迁移到另一台电脑
A: 复制整个文件夹，**不要**只复制某些子目录

### Q: 完全卸载某个环境
A: 删掉整个文件夹即可（**0 残留**）

### Q: 在没装 WebView2 的机器上能用吗
A: 不行。WebView2 是必须的（启动器 UI 依赖它）
`;
  writeFileSync(join(portableDir, "README.md"), readme);
  log("info", "已写 README.md");

  // 写 .gitignore（如果用户用 git 管理）
  const gitignore = `# 装好后才有这些大目录
ComfyUI/
data/
.boundlaunch/

# launcher 自身文件（可能不提交）
BoundLaunch.exe
BoundLaunch.dll
`;
  writeFileSync(join(portableDir, ".gitignore"), gitignore);
  log("info", "已写 .gitignore");

  return portableDir;
}

// ============================================================================
// 阶段 3: 打包 zip
// ============================================================================

async function packZip(portableDir, version) {
  log("info", "===== 阶段 3/5: 打包 zip =====");
  const zipPath = join(DIST_DIR, `BoundLaunch-portable-v${version}.zip`);

  if (existsSync(zipPath)) {
    rmSync(zipPath, { force: true });
  }

  // 用 PowerShell 的 Compress-Archive 打包（Windows 自带，无需第三方库）
  const portableDirName = basename(portableDir);
  const psCommand = `Compress-Archive -Path "${portableDirName}\\*" -DestinationPath "${basename(zipPath)}" -Force`;
  const result = spawnSync("powershell", ["-NoProfile", "-Command", psCommand], {
    cwd: DIST_DIR,
    stdio: "inherit",
  });
  if (result.status !== 0) {
    log("error", "zip 打包失败");
    process.exit(1);
  }

  log("success", `已生成: ${zipPath}`);
  log("info", `大小: ${formatSize(statSync(zipPath).size)}`);
  return zipPath;
}

// ============================================================================
// 阶段 4: 计算 SHA256
// ============================================================================

function computeHash(filePath) {
  const { createHash } = require("node:crypto");
  const hash = createHash("sha256");
  const data = readFileSync(filePath);
  hash.update(data);
  return hash.digest("hex");
}

// ============================================================================
// 主流程
// ============================================================================

async function main() {
  const version = getVersion();
  log("info", `===== BoundLaunch 绿色版打包 (v${version}) =====`);
  log("info", `项目根: ${PROJECT_ROOT}`);
  log("info", `目标平台: ${process.platform}/${process.arch}`);

  if (SKIP_BUILD) {
    log("warn", "跳过 cargo build（--skip-build）");
  } else {
    buildRust();
  }

  const portableDir = preparePortableDir(version);
  const zipPath = await packZip(portableDir, version);

  log("info", "===== 阶段 4/5: 计算 SHA256 =====");
  const hash = computeHash(zipPath);
  log("info", `SHA256: ${hash}`);

  log("info", "===== 阶段 5/5: 报告 =====");
  log("success", "✓ 绿色版打包完成");
  log("info", `产物: ${zipPath}`);
  log("info", `大小: ${formatSize(statSync(zipPath).size)}`);
  log("info", `SHA256: ${hash}`);
  log("info", "");
  log("info", "解压后双击 BoundLaunch.exe 即可使用");
  log("info", "复制整个文件夹即可创建独立环境");
}

main().catch((err) => {
  log("error", err.message);
  console.error(err);
  process.exit(1);
});
