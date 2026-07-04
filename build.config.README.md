# myComfyUI 编译配置说明

完整规范：[pr/05-依赖与启动参数.md §5](./pr/05-依赖与启动参数.md) / [pr/08-验证清单.md §4.4](./pr/08-验证清单.md)

## `build_jobs` —— 编译并行度

控制 `cargo test` 与 `npx tauri build`（内部 `cargo build`）的并行任务数。

### 优先级

```
环境变量 BUILD_JOBS  >  build.config.json  >  自动检测 (os.cpus().length, 上限 16)
```

### 三种配置方式

**1. 环境变量（推荐临时）**

```cmd
:: Windows CMD
set BUILD_JOBS=8
build.bat

:: PowerShell
$env:BUILD_JOBS=8; node build.mjs

:: Linux / macOS
BUILD_JOBS=8 node build.mjs --skip-tests
```

**2. 项目配置文件（推荐持久）**

编辑项目根 [build.config.json](./build.config.json)：

```json
{
  "build_jobs": 8
}
```

**3. 自动检测（零配置）**

不设任何值时，build.mjs 自动读取 `os.cpus().length`，硬上限 16。

### 推荐取值

| 机器规格 | build_jobs |
|---|---|
| 4 核 / 8 GB | 4 |
| 8 核 / 16 GB | 8 |
| 16 核 / 32 GB | 12-16 |
| **32+ 核 / 64+ GB** | **8-12**（不要用全部，避免 OOM） |
| CI runner（4 核 / 7 GB） | 2-4 |

### 生效确认

build.mjs 启动时打印：

```
[INFO] Build Jobs: 16 (来源: build.config.json)
```

### 故障排查

| 症状 | 处理 |
|---|---|
| OOM / link 失败 | 调小 `build_jobs`（如 4） |
| 编译很慢 | 调大 `build_jobs` |
| 配置不生效 | 确认 `build.config.json` 在**项目根目录** |
| Windows 环境变量不生效 | CMD 用 `set`，PowerShell 用 `$env:` |
