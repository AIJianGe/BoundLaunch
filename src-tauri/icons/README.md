# Icons

无界启动器 (BoundLaunch) 应用图标集。

## 文件清单

| 文件 | 用途 |
|---|---|
| `source-logo-real.png` | 品牌 logo 源文件（1832×1832 PNG），用于重新生成各尺寸 |
| `icon.ico` | Windows 应用图标（含多尺寸） |
| `icon.icns` | macOS 应用图标 |
| `icon.png` | Linux 应用图标（512×512） |
| `32x32.png` / `64x64.png` / `128x128.png` / `128x128@2x.png` | 各平台 PNG 尺寸 |
| `Square*Logo.png` / `StoreLogo.png` | Windows Store 应用图标 |
| `ios/AppIcon-*.png` | iOS 应用图标 |
| `android/mipmap-*/ic_launcher*.png` | Android 应用图标 |

## 重新生成

如需替换 logo，将新源文件命名为 `source-logo-real.png`（建议 ≥1024×1024 PNG，透明背景），然后执行：

```bash
npx @tauri-apps/cli icon source-logo-real.png --output .
```

生成后会覆盖当前所有尺寸文件。生成后请同步更新 `public/icon.png` 与 `public/favicon-32.png`（用于 webview favicon）。

## 设计语言

- 主题：白色火箭 + 圆形轨道环，象征"突破边界、启动"
- 背景：深海军蓝 (#1a365d) → 亮青色 (#24C8DB) 渐变
- 风格：扁平化、几何感、高对比度
- 与 UI 中的 🚀 emoji 设计语言保持一致
