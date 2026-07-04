# 无界启动器 (BoundLaunch)

> **跨平台、零依赖、开箱即用的 [ComfyUI](https://github.com/comfyanonymous/ComfyUI) 桌面启动器**
>
> 把 Python 环境管理、核心版本切换、CPU/GPU 启动、插件管理、模型目录配置——全部 GUI 化。

[![Tauri](https://img.shields.io/badge/Tauri-2.x-24C8DB?logo=tauri)](https://tauri.app)
[![Vue](https://img.shields.io/badge/Vue-3.5-42B883?logo=vue.js)](https://vuejs.org)
[![Rust](https://img.shields.io/badge/Rust-1.80+-DEA584?logo=rust)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20Linux-lightgrey)](#-支持的平台)

[English](#-english-version-coming-soon) | **简体中文**

---

## ✨ 核心特性

| 模块 | 能力 |
|---|---|
| 🐍 **Python 环境** | 通过 `uv` 自动创建/重建 venv，灵活切换 Python 与 PyTorch（含 CUDA）版本，启动前自动校验环境一致性 |
| 🎯 **核心管理** | 一键克隆 ComfyUI 仓库、查看/切换 stable/nightly tag，启动前检查进程互斥 |
| 🧩 **插件管理** | custom_nodes 的增/改/卸/启停，支持 Git 仓库与 zip 包，含回收站机制 |
| 📁 **模型路径** | GUI 配置 `extra_model_paths.yaml`，支持方案 A 多根目录模式 + 自动扫描子目录 |
| 🚀 **进程管理** | 启动/停止 ComfyUI 主进程，实时日志流（环形缓冲 + 背压控制），健康检查 + 崩溃自动恢复 |
| 🔍 **环境探查** | GPU 型号/显存/CUDA 版本识别，关键依赖版本探测，状态卡片展示 |
| 📊 **任务调度** | 统一管理长任务（>1s）的提交/进度推送/取消/历史，优先级队列 + 并发上限 |
| 💾 **日志持久化** | SQLite 存储所有日志 + 任务历史，tags 内存缓存 + LRU 淘汰，可配置保留策略 |
| 🎨 **桌面集成** | 任务栏托盘图标、关闭窗口到托盘、快捷键、全局错误页、首次运行向导 |

---

## 📸 截图

> 截图待补：启动页 / 模型路径页 / 插件管理页 / 任务进度中心 / 核心版本切换

<details>
<summary>主要页面预览（占位）</summary>

- **启动页**：状态卡片 + GPU/CPU 模式 + 基础/高级参数 + 实时命令预览
- **模型路径页**：多根目录配置 + 子目录扫描 + 文件预览
- **核心版本页**：tag 列表 + 4 状态切换（当前/可用/下载中/冲突）
- **插件管理页**：custom_nodes CRUD + 安装进度 + 回收站
- **设置页**：Python 版本切换 5 步进度 + venv 重建
- **任务进度中心**：进行中/排队/历史 3 列表 + 失败详情展开
- **关于页**：版本信息 + 检查更新

</details>

---

## 🚀 快速开始

### 前置条件

| 工具 | 最低版本 | 说明 | 安装 |
|---|---|---|---|
| Node.js | 18+ | 前端构建 | <https://nodejs.org/> |
| Rust | 1.80+ | 后端编译 | <https://rustup.rs/> |
| npm | 9+ | 依赖管理（Node 自带） | — |
| **Windows** | — | Visual Studio Build Tools 2022 + C++ 工作负载 | <https://visualstudio.microsoft.com/visual-cpp-build-tools/> |
| **Linux** | — | `libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev patchelf` | 见下方命令 |

**Linux 一键安装系统依赖**：

```bash
# Ubuntu / Debian
sudo apt install -y libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev patchelf

# Fedora
sudo dnf install -y webkit2gtk4.1-devel gtk3-devel libappindicator-gtk3-devel librsvg2-devel patchelf

# Arch
sudo pacman -S --needed webkit2gtk4.1 gtk3 libappindicator-gtk3 librsvg patchelf
```

### 安装

```bash
git clone https://github.com/<your-name>/BoundLaunch.git
cd BoundLaunch
npm install
```

### 启动开发模式

```bash
npm run tauri dev
```

首次启动会触发 Vite + Tauri + Rust 的完整编译，约 5-10 分钟（取决于机器配置）。

### 打包发布

**Windows**（双击即可）：

```cmd
build.bat
```

**Linux / macOS**：

```bash
chmod +x build.sh
./build.sh
```

**或直接调用**：

```bash
# 完整打包（含测试）
npm run package

# 快速打包（跳过测试，推荐开发迭代）
npm run package:fast

# Windows 安装包
npm run package:nsis    # NSIS .exe
npm run package:msi     # MSI 安装包

# 调试构建
npm run package:debug
```

产物位置：`src-tauri/target/release/bundle/`

---

## 🛠️ 编译配置（build_jobs）

> 高核心数机器（≥32 核）调小 `build_jobs` 可避免 OOM 与磁盘 I/O 抖动。

**三种方式（优先级递减）**：

```cmd
:: 1. 环境变量（临时）
set BUILD_JOBS=8
build.bat
```

```bash
# 2. 环境变量（Linux / macOS）
BUILD_JOBS=8 node build.mjs --skip-tests
```

```json
// 3. 项目配置文件（持久，推荐）——  build.config.json
{
  "build_jobs": 8
}
```

**推荐取值**：

| 机器规格 | build_jobs |
|---|---|
| 4 核 / 8 GB | 4 |
| 8 核 / 16 GB | 8 |
| 16 核 / 32 GB | 12-16 |
| **32+ 核 / 64+ GB** | **8-12**（不要用全部） |

完整规范：[build.config.README.md](./build.config.README.md)

---

## 📁 项目结构

```
boundlaunch/
├── src/                          # 前端（Vue 3 + TypeScript + Naive UI）
│   ├── components/               # 通用组件
│   ├── views/                    # 页面（启动/设置/插件/模型/关于…）
│   ├── composables/              # 组合式 API 封装
│   ├── stores/                   # Pinia 状态管理
│   └── styles/                   # 全局样式
│
├── src-tauri/                    # 后端（Rust + Tauri 2）
│   ├── src/
│   │   ├── commands/             # Tauri command 入口
│   │   ├── config/               # TOML 配置 + 迁移
│   │   ├── core_manager/         # ComfyUI 核心（git / tag）
│   │   ├── env_inspector/        # 环境探查
│   │   ├── log_store/            # SQLite 日志 + 任务历史
│   │   ├── model_path/           # extra_model_paths.yaml
│   │   ├── plugin_manager/       # custom_nodes CRUD
│   │   ├── process_launcher/     # ComfyUI 启停 + 健康检查
│   │   ├── python_env/           # uv / venv / torch
│   │   └── task_scheduler/       # 长任务调度
│   ├── capabilities/             # Tauri 权限声明
│   ├── icons/                    # 应用图标
│   ├── tauri.conf.json
│   └── Cargo.toml
│
├── build.mjs                     # 跨平台打包脚本（核心）
├── build.bat                     # Windows 打包启动器
├── build.sh                      # Linux 打包启动器
├── build.config.json             # 编译并行度配置
├── build.config.example.json     # 配置模板
├── build.config.README.md        # 编译配置详细文档
│
├── package.json
├── vite.config.ts
├── tsconfig.json
├── index.html
├── .gitignore
├── LICENSE
└── README.md                     # 本文件
```

---

## 🌐 支持的平台

| 平台 | 状态 | 安装包格式 |
|---|---|---|
| **Windows 10 / 11** | ✅ 完整支持 | `.msi` / `.exe` (NSIS) |
| **Linux (Ubuntu 22.04+)** | ✅ 完整支持 | `.deb` / `.AppImage` |
| **Linux (Fedora / Arch)** | ✅ 完整支持 | `.AppImage`（deb 可手动打包） |
| **macOS (Intel + Apple Silicon)** | ✅ 完整支持 | `.dmg` / `.app` |

### macOS 备注

- **前置条件**：Xcode Command Line Tools（`xcode-select --install`）
- **未签名 .dmg**：用户首次双击会被 Gatekeeper 拦截，需右键 → 打开 → 仍要打开
- **正式分发**：建议配置 Apple Developer ID 代码签名 + 公证，详见 [pr/08-验证清单.md §4.3](./pr/08-验证清单.md)
- **Apple Silicon**：默认按本机架构打包；跨架构需 `--target aarch64-apple-darwin` 或 `x86_64-apple-darwin`

> macOS 需要本机有 Xcode Command Line Tools；Linux 不同发行版的 Tauri 依赖差异较大，遇到问题请提交 issue。

---

## 🧰 技术栈

### 前端
- **Vue 3.5** + Composition API
- **TypeScript 5.5**（严格模式）
- **Naive UI 2.x**（组件库）
- **Pinia 2.x**（状态管理）
- **Vue Router 4.x**
- **Vite 5.x**（构建）

### 后端
- **Tauri 2.x**（桌面运行时）
- **Rust 1.80+**（edition 2021）
- **Tokio 1.x**（异步运行时）
- **SQLite (sqlx 0.8)**（本地持久化）
- **git2-rs**（ComfyUI / 插件克隆）
- **reqwest + rustls**（HTTP 客户端）
- **chrono / uuid / arc-swap / parking_lot / rayon / dashmap**（基础设施）

### 设计模式
- **Builder**：Config / Task 构造
- **Repository**：LogStore / ConfigService
- **Command**：TaskScheduler / Tauri commands
- **Strategy**：EnvironmentChecker / BuildPipeline
- **Observer**：事件总线 + 前端订阅
- **Semaphore**：任务并发上限
- **State Machine**：ProcessLauncher 6 态
- **Template Method**：BuildPipeline 阶段编排

---

## 📊 路线图

### ✅ Phase 0-2（MVP - 当前）
- 9 模块基础架构（Config / PythonEnv / Core / Plugin / ModelPath / ProcessLauncher / EnvInspector / TaskScheduler / LogStore）
- 首次运行向导（onboarding）
- 启动页 + 设置页 + 模型路径页 + 插件管理页 + 关于页 + 任务进度中心 + 日志页
- 跨平台打包（Windows / Linux / macOS）
- 编译并行度全局配置

### 🚧 后续规划
- 应用内自动更新（updater 插件）
- macOS 代码签名 + 公证自动化（CI 集成）
- 模型下载（来自 Civitai / Hugging Face）
- 工作流导出/导入
- 插件市场
- 多语言（i18n）

---

## 🤝 贡献

欢迎贡献代码、提交 issue、改进文档！

- **Bug 报告**：[GitHub Issues](../../issues)（使用 issue 模板）
- **功能建议**：[GitHub Discussions](../../discussions)
- **Pull Request**：fork → 创建特性分支 → 提交 → 创建 PR
  - 前端改动请确保 `npm run typecheck` 通过
  - Rust 改动请确保 `cargo check --all-targets` 通过
  - 提交信息请使用 [Conventional Commits](https://www.conventionalcommits.org/) 规范

### 开发命令速查

```bash
npm run dev              # Vite 开发服务器（仅前端）
npm run typecheck        # vue-tsc 类型检查
npm run build            # 前端构建
npm run tauri dev        # Tauri 开发模式（含 Rust 热重启）
npm run package          # 完整打包
npm run package:fast     # 跳过测试快速打包
```

---

## 📜 许可证

本项目基于 [MIT License](./LICENSE) 开源。

```
MIT License

Copyright (c) 2026 BoundLaunch Contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

---

## 🙏 致谢

- **[ComfyUI](https://github.com/comfyanonymous/ComfyUI)** — 强大的 Stable Diffusion 图形化工作流引擎
- **[Tauri](https://tauri.app)** — 现代化的 Rust 跨平台桌面应用框架
- **[Naive UI](https://www.naiveui.com)** — 优雅的 Vue 3 组件库
- **[uv](https://github.com/astral-sh/uv)** — 极速的 Python 包管理器
- 所有贡献者与用户的支持 ❤️

---

## 📮 联系方式

- **GitHub Issues**：[提交 Bug / 功能建议](../../issues)
- **GitHub Discussions**：[讨论与交流](../../discussions)

> ⭐ 如果这个项目对你有帮助，欢迎 Star！
