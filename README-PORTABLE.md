# BoundLaunch 绿色版（Portable）使用指南

> **目标**：让 BoundLaunch.exe 像 U 盘一样便携——解压即用，复制即多环境，删除即零残留。

---

## 快速开始

### 1. 获取绿色版

下载 `BoundLaunch-portable-v0.1.0.zip`，解压到任意目录，例如：

```
D:\AIWork\BoundEnv-A\
├── BoundLaunch.exe
├── BoundLaunch.dll
├── resources\uv\
├── launcher-portable.dat
├── README.md
└── .gitignore
```

### 2. 首次启动

双击 `BoundLaunch.exe`，按引导完成 ComfyUI 安装。

> 首次启动会自动生成 `launcher-portable.dat`（如果你解压时已经包含，会被复用）。

### 3. 创建新环境

**复制整个目录即可得到完全独立的环境**：

```powershell
# 在 PowerShell 中
Copy-Item -Recurse "D:\AIWork\BoundEnv-A" "D:\AIWork\BoundEnv-B"
```

之后修改 `D:\AIWork\BoundEnv-B\launcher-portable.dat`：

```toml
name = "EnvB-SDXL"
port = 8189
```

这样两个环境可以同时运行（端口不同），互不干扰。

---

## 目录结构

```
BoundLaunch-v0.1.0/
├── BoundLaunch.exe              ← 启动器主程序
├── BoundLaunch.dll              ← 启动器库
├── resources\uv\                ← uv sidecar（Python 包管理加速）
├── launcher-portable.dat        ← 环境配置（关键！）
│
├── ComfyUI\                     ← ComfyUI 核心（首次启动后克隆）
│   ├── main.py
│   ├── custom_nodes\            ← 默认情况下插件装这里
│   └── models\
│
├── data\
│   └── venv\                    ← Python 虚拟环境（含 torch 约 4.2 GB）
│
└── .boundlaunch\                ← launcher 私有数据
    ├── launcher.db              ← SQLite（任务历史、日志）
    └── logs\                    ← 启动器日志
```

---

## `launcher-portable.dat` 详解

这是绿色版的核心配置文件，每次启动器启动时读取。

### 默认内容

```toml
version = 1
name = ""               # 空 = 用目录名兜底
port = 8188             # ComfyUI 监听端口
override_base_directory = false

[paths]
comfyui = "ComfyUI"
venv = "data/venv"
custom_nodes = "ComfyUI/custom_nodes"
models = "ComfyUI/models"
boundlaunch_data = ".boundlaunch"
```

### 字段说明

| 字段 | 含义 | 默认值 | 注意事项 |
|------|------|--------|----------|
| `name` | 环境名（用于日志、UI、数据库命名） | 空 → 用目录名 | 多个环境用不同 name 区分 |
| `port` | ComfyUI 启动端口 | 8188 | 多环境时改这个避免冲突 |
| `override_base_directory` | 是否给 ComfyUI 传 `--base-directory` | false | 留空即可，自动推断 |
| `paths.comfyui` | ComfyUI 仓库目录 | `ComfyUI` | 相对当前目录 |
| `paths.venv` | Python 虚拟环境 | `data/venv` | 切换 ComfyUI 版本不影响 venv |
| `paths.custom_nodes` | 插件目录 | `ComfyUI/custom_nodes` | 可改到 ComfyUI 外 |
| `paths.models` | 模型目录 | `ComfyUI/models` | 可改到 ComfyUI 外 |
| `paths.boundlaunch_data` | launcher 私有数据 | `.boundlaunch` | 含 SQLite + 日志 |

### 路径解析规则

- **相对路径**（如 `ComfyUI`）→ 相对 **当前 `BoundLaunch.exe` 所在目录**
- **绝对路径**（如 `D:\SharedCustomNodes`）→ 直接用，跳过相对解析

---

## 常见场景

### 场景 1：多环境完全隔离（推荐）

**目标**：同时跑 SD 1.5 和 SDXL 两套环境，互不影响。

```powershell
# 创建两个环境
Copy-Item -Recurse "D:\EnvTemplate" "D:\Env-SD15"
Copy-Item -Recurse "D:\EnvTemplate" "D:\Env-SDXL"

# 配置端口
notepad D:\Env-SD15\launcher-portable.dat   # port = 8188
notepad D:\Env-SDXL\launcher-portable.dat   # port = 8189
```

启动 `D:\Env-SD15\BoundLaunch.exe` → ComfyUI 跑在 `http://127.0.0.1:8188`
启动 `D:\Env-SDXL\BoundLaunch.exe` → ComfyUI 跑在 `http://127.0.0.1:8189`

**两套环境**：
- 独立的 `venv`（不同 torch 版本/包）
- 独立的 `custom_nodes`（不同插件组合）
- 独立的 `launcher.db`（独立的任务历史）
- 共享 `BoundLaunch.exe` 升级时复制一次

### 场景 2：共享模型目录（节省磁盘）

如果两个环境的模型一样（不切换 ComfyUI 版本时常见），可以让两个环境指向同一份 models：

```toml
# D:\Env-SD15\launcher-portable.dat
[paths]
models = "D:/SharedModels"
```

效果：`ComfyUI/models` 路径下被启动器自动管理，文件实际存储在 `D:\SharedModels`。

### 场景 3：custom_nodes 放 ComfyUI 外（高级）

适合"多环境共享同一份插件"的场景：

```toml
# D:\Env-A\launcher-portable.dat
[paths]
custom_nodes = "D:/SharedCustomNodes"
```

启动器会自动给 ComfyUI 加 `--base-directory D:/SharedCustomNodes`，ComfyUI 内部 `folder_paths.py` 的 `base_path` 切换到该目录，`custom_nodes / models / input / output` 都从这下面找。

**重要**：只有当 `custom_nodes` 在 `comfyui` 目录外时，启动器才会自动传 `--base-directory`。如果你设置 `override_base_directory = true` 但 `custom_nodes` 实际在 `comfyui` 内，ComfyUI 会找不到部分路径。

### 场景 4：迁移到新电脑

把整个绿色版目录复制到新电脑，双击 `BoundLaunch.exe` 即可。

**注意**：
- 新电脑需要装 [WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/)
- 如果新电脑没装 VC++ 2015-2022 Redistributable，启动器会**主动检测并弹出友好提示**

### 场景 5：完全卸载

```powershell
Remove-Item -Recurse -Force "D:\Env-SD15"
```

**零残留**——不会写注册表，不写 `%APPDATA%`，不留下任何痕迹。

---

## 打包新版绿色版

### 从源代码构建

```bash
# 1. 准备环境（首次）
npm install
rustup target add x86_64-pc-windows-msvc

# 2. 一键打包
build.bat           # Windows
# 选择 [6] Portable
```

### 产物

- `dist\BoundLaunch-v0.1.0\`   ← 绿色版目录（已带 portable.dat 和 README）
- `dist\BoundLaunch-portable-v0.1.0.zip`   ← 分发用的压缩包

### 跳过 cargo build

如果 `target\release\BoundLaunch.exe` 已存在：

```bash
node scripts\build_portable.mjs --skip-build
```

---

## 常见问题

### Q1: 双击 `BoundLaunch.exe` 没反应

**A**: 大概率是缺 VC++ 运行时。启动器会**主动**弹中文错误框提示下载链接。如果没有弹框（极少见），手动安装 [VC++ 2015-2022 Redistributable x64](https://aka.ms/vs/17/release/vc_redist.x64.exe)。

### Q2: 启动器报 "WebView2 not found"

**A**: 装 [WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/)（Win11 默认有，Win10 需要手动装）。

### Q3: 端口被占用

**A**: 编辑 `launcher-portable.dat` 改 `port` 字段到空闲端口（8188 → 8189 → ...）。`netstat -ano | findstr :8188` 查占用进程。

### Q4: 复制环境后启动报 "ComfyUI not found"

**A**: 确保整个目录复制完整，包括隐藏的 `.boundlaunch\`。如果只复制了部分目录，启动器会因为找不到 ComfyUI 而报错。

### Q5: 多个环境的数据库会冲突吗

**A**: 不会。每个环境的 SQLite 在各自的 `.boundlaunch\launcher.db`，完全独立。日志、任务历史、插件列表都是隔离的。

### Q6: 可以把 `data\venv` 放到 SSD 吗

**A**: 可以。编辑 `launcher-portable.dat`：

```toml
[paths]
venv = "E:/FastVenv"    # 绝对路径
```

venv 仍然在 `E:/FastVenv`（不要去改 `ComfyUI` 内），ComfyUI 启动时通过 `<venv_path>\Scripts\python.exe` 调用。

### Q7: 我能不能同时跑两个 ComfyUI 共享模型

**A**: 可以。两个环境的 `launcher-portable.dat` 都设置 `models = "D:/SharedModels"`，端口不同。启动器会管理好软链接，ComfyUI 看到的 `<comfyui>/models` 实际指向 `D:/SharedModels`。

**警告**：不要让两个 ComfyUI **同时写**模型目录（如同时下载模型），会产生文件锁冲突。下载时只开一个环境。

### Q8: 升级 BoundLaunch.exe 但保留环境和数据

```powershell
# 用新版覆盖 exe + dll 即可，其他都不动
Copy-Item "D:\NewBound\BoundLaunch.exe" "D:\Env-SD15\BoundLaunch.exe" -Force
Copy-Item "D:\NewBound\BoundLaunch.dll" "D:\Env-SD15\BoundLaunch.dll" -Force
```

venv、custom_nodes、ComfyUI、SQLite 全部保留。

---

## 与传统安装版的区别

| 维度 | 绿色版 (Portable) | 传统安装版 (MSI/NSIS) |
|------|-------------------|---------------------|
| 安装 | 解压即用 | 需要运行安装器 |
| 启动器位置 | `<解压目录>\BoundLaunch.exe` | `C:\Program Files\BoundLaunch\` |
| 配置/数据位置 | `<解压目录>\.boundlaunch\` | `%APPDATA%\boundlaunch\` |
| 升级 | 覆盖 exe + dll | 重新运行安装器 |
| 卸载 | 删目录 | 控制面板卸载 |
| 多环境 | 复制目录 | 需要新装一次 |
| 注册表 | 不写 | 写 |
| 系统服务 | 不注册 | 可能注册 |

**核心区别**：传统安装版写到 `%APPDATA%`，绿色版一切都在 exe 旁边。

---

## 开发者参考

详见 `PR/03-模块设计/01-Config.md` §"Portable 模式路径解析"。

关键文件：
- `src-tauri\src\paths\env_paths.rs` — `launcher-portable.dat` 解析逻辑
- `src-tauri\src\config\service.rs::apply_default_paths` — 启动时把 env_paths 注入 config
- `src-tauri\src\process_launcher\command_builder.rs` — `--base-directory` 参数生成
- `src-tauri\tauri.conf.json::bundle.targets` — 含 `app` target（产出 unpacked）
- `scripts\build_portable.mjs` — 绿色版打包脚本
