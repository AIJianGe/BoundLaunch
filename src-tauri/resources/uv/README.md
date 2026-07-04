# uv Sidecar 资源目录

本目录存放 **uv** 二进制（Python 包管理工具），由 launcher 启动时释放到用户数据目录使用。

## 文件命名规范

文件名必须符合 Tauri 2 的 `target-triple` 约定：

| 平台 | target triple | 文件名 |
| --- | --- | --- |
| Windows x86_64 | `x86_64-pc-windows-msvc` | `uv-x86_64-pc-windows-msvc.exe` |
| macOS x86_64 (Intel) | `x86_64-apple-darwin` | `uv-x86_64-apple-darwin` |
| macOS aarch64 (Apple Silicon) | `aarch64-apple-darwin` | `uv-aarch64-apple-darwin` |
| Linux x86_64 | `x86_64-unknown-linux-gnu` | `uv-x86_64-unknown-linux-gnu` |

## 自动下载

运行仓库根目录的 `scripts/fetch-uv.ps1` 脚本下载本机所需平台的 uv 二进制：

```powershell
cd D:\AIWork\myComfyui
.\scripts\fetch-uv.ps1
```

脚本会：
1. 读取本机 `rustc -vV` 拿到 host triple
2. 从 `https://github.com/astral-sh/uv/releases/latest` 下载对应压缩包
3. 解压出 uv 二进制到本目录

## 手动下载

1. 访问 [uv GitHub Releases](https://github.com/astral-sh/uv/releases/latest)
2. 下载本机平台的归档（zip for Windows / tar.gz for Unix）
3. 解压出 `uv` / `uv.exe`
4. 重命名为上面的文件名，复制到本目录

## 打包说明

`tauri.conf.json` 的 `bundle.resources` 配置了 `resources/uv/*`，Tauri 打包时会自动把本目录下所有文件打入安装包。

## 用户机器上的使用流程

1. 用户首次启动 launcher
2. Rust 端从 `resource_dir/uv/uv-{triple}.exe` 复制到 `%APPDATA%/boundlaunch/uv/uv.exe`
3. 后续 `uv` 调用都用 `%APPDATA%/boundlaunch/uv/uv.exe` 绝对路径
4. 用户无需安装 uv

## 版本

当前固定版本：`0.4.18`（如需升级改 `scripts/fetch-uv.ps1` 顶部的 `$UV_VERSION`）
