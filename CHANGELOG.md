# 变更日志

所有对 BoundLaunch 项目的显著修改都会记录在此文件中。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [未发布]

## [0.0.1] - 2026-07-11

### 新增

- **v0.0.1 首次发布**：ComfyUI 桌面启动器
- 一键启动 ComfyUI（自动检测 Python / venv / torch）
- ComfyUI-Manager 自动重启协议支持（`__COMFY_CLI_SESSION__`）
- 多实例隔离（绿色版复制目录自动隔离 session）
- 自动诊断 / 自动修复 / 启动失败详细错误对话框
- 启动进度条 + 取消启动 + 慢启动预警（60s/180s 阈值）
- 启动后自动打开浏览器
- ComfyUI 核心版本切换（v3.1 决策 6：自动回滚）
- 插件管理（Git 仓库安装 / 启用 / 更新 / 卸载 + 启动加载）
- 内置 uv 包管理器（无需用户手动安装）
- ComfyUI 仓库国内镜像加速
- 一键"安装模型"快捷入口
- 主题切换（light / dark / auto）
- 错误面板（最近 20 条业务错误 + 菜单红点 + 过滤 + 导出）
- 运行日志（实时 stdout/stderr / 自动滚动 / 过滤 / 导出）
- 终端（xterm.js / ANSI 颜色）
- 安装日志（业务事件流 / 按阶段过滤）
- 路径选择器（Linux/Windows 跨平台统一）
- 多 GPU 选择（Phase 5）
- F24 5 步退出流程（防重入 + 30s 超时兜底 + 进程组清理）
- 系统托盘（最小化到托盘 + 托盘菜单退出）
- GPLv3 开源协议

### 已知问题

- 启动器不支持 Windows 7 及以下（需要 Windows 10+）
- macOS arm64 需 ≥ 11.0
- Linux 仅测试了 glibc 发行版（musl 可能需自编译）

### 升级说明

- 这是首个版本，无升级路径
- 后续版本可通过 AboutPage → "检查更新"自动升级

[未发布]: https://github.com/AIJianGe/BoundLaunch/compare/v0.0.1...HEAD
[0.0.1]: https://github.com/AIJianGe/BoundLaunch/releases/tag/v0.0.1
