; v1.8 / F38：Portable 模式 - NSIS 安装器自定义钩子
;
; 钩子说明（Tauri 2 NSIS）：
; - NSIS_HOOK_PREINSTALL   : 复制文件、设置注册表键、创建快捷方式之前
; - NSIS_HOOK_POSTINSTALL  : 复制文件、设置注册表键、创建快捷方式之后
; - NSIS_HOOK_PREUNINSTALL : 删除任何文件、注册表键、快捷方式之前
; - NSIS_HOOK_POSTUNINSTALL: 删除所有文件、注册表键、快捷方式之后
;
; 用途：
; 1. 安装后写一个 PORTABLE_INFO.txt 提示用户数据存在哪
; 2. 卸载时清理该提示文件
;
; 注意：这些宏必须存在于 Tauri 生成 installer.nsi 时被 include 的 .nsh 文件中
; 详见 tauri.conf.json 的 bundle.windows.nsis.installer_hooks 配置

; ============== 安装完成后钩子 ==============
!macro NSIS_HOOK_POSTINSTALL
  ; 写一个 PORTABLE_INFO.txt 提示用户 portable 模式
  ; 位置：$INSTDIR\PORTABLE_INFO.txt
  ; INSTDIR 由 NSIS 自动定义，指向用户选择的安装目录 + productName
  FileOpen $0 "$INSTDIR\PORTABLE_INFO.txt" w
  FileWrite $0 "BoundLaunch Portable Mode (v1.8 / F38)$\r$\n"
  FileWrite $0 "===========================================$\r$\n"
  FileWrite $0 "$\r$\n"
  FileWrite $0 "Data location: $INSTDIR\data\$$\r$\n"
  FileWrite $0 "$\r$\n"
  FileWrite $0 "This launcher uses Portable Mode. All data (config, logs, venv, transformers cache) is stored in the data\ subfolder next to the executable, NOT in the system APPDATA directory.$\r$\n"
  FileWrite $0 "$\r$\n"
  FileWrite $0 "Tips:$\r$\n"
  FileWrite $0 "  - To install multiple isolated copies, copy the entire BoundLaunch folder to different locations. Each copy will have its own data.$\r$\n"
  FileWrite $0 "  - To completely uninstall, delete the entire BoundLaunch folder (including data\).$\r$\n"
  FileWrite $0 "  - To use a custom data directory, set the BOUND_LAUNCH_DATA_DIR environment variable.$\r$\n"
  FileWrite $0 "  - You can check the data location in the launcher's Settings page.$\r$\n"
  FileClose $0
!macroend

; ============== 卸载前钩子 ==============
!macro NSIS_HOOK_PREUNINSTALL
  ; 卸载时主动询问用户：是否保留 data 目录（防止误删用户下载的模型 / 训练好的 workflow）
  ; 默认行为：保留（避免误删），用户可手动删除
  ; 注：Tauri 默认 uninstall 会删除 INSTDIR 下所有内容，所以这里不主动删
  ; 也不需要写额外文件
!macroend
