#!/usr/bin/env bash
# ============================================================================
#  无界启动器 (BoundLaunch) 打包启动器 (Linux / macOS)
#
#  跨平台说明：
#    本脚本同时支持 Linux 与 macOS。通过 `uname -s` 检测当前平台，
#    走不同的依赖检查分支：
#      - Linux:  ldconfig + .so 检查 webkit2gtk / gtk3 等系统库
#      - macOS:  检查 Xcode Command Line Tools（xcode-select -p）
#
#  双击运行说明：
#    首次使用：右键 build.sh → 属性 → 允许作为程序执行
#    或在终端执行：chmod +x build.sh
#
#  实际工作交给 build.mjs，本脚本仅负责环境检查 + 调用。
#  详见 pr/08-验证清单.md §4 跨平台打包（§4.2 Linux / §4.3 macOS）
# ============================================================================

# 切换到脚本所在目录（双击时工作目录可能是 ~）
cd "$(dirname "$0")" || exit 1

# --- 0. 平台检测 ---
OS_NAME=$(uname -s)  # Linux: "Linux"  /  macOS: "Darwin"
case "$OS_NAME" in
    Linux*)  PLATFORM_LABEL="Linux"  ;;
    Darwin*) PLATFORM_LABEL="macOS"  ;;
    *)
        echo "[错误] 不支持的平台: $OS_NAME（仅支持 Linux / macOS）"
        exit 1
        ;;
esac

# 颜色定义
if [ -t 1 ] && [ -z "$NO_COLOR" ]; then
    RED='\033[31m'
    GREEN='\033[32m'
    YELLOW='\033[33m'
    BLUE='\033[34m'
    CYAN='\033[36m'
    BOLD='\033[1m'
    RESET='\033[0m'
else
    RED=''; GREEN=''; YELLOW=''; BLUE=''; CYAN=''; BOLD=''; RESET=''
fi

echo
echo "============================================================"
echo -e "${BOLD}${CYAN}  无界启动器 打包启动器 ($PLATFORM_LABEL)${RESET}"
echo "  实际工作脚本: build.mjs"
echo "  平台检测: OS_NAME=$OS_NAME"
echo "============================================================"
echo

# --- 1. 检查脚本执行权限（自动 chmod）---
if [ ! -x "$0" ]; then
    echo -e "${YELLOW}[警告]${RESET} 脚本缺少执行权限，尝试自动添加..."
    chmod +x "$0" 2>/dev/null
    if [ ! -x "$0" ]; then
        echo -e "${RED}[错误]${RESET} 无法添加执行权限"
        echo "请手动执行: chmod +x build.sh"
        read -p "按回车退出..."
        exit 1
    fi
    echo -e "${GREEN}[信息]${RESET} 已自动添加执行权限"
    echo
fi

# --- 2. 检查 Node.js ---
if ! command -v node &> /dev/null; then
    echo -e "${RED}[错误]${RESET} 未检测到 Node.js"
    echo
    echo "请先安装 Node.js 18+："
    if [ "$PLATFORM_LABEL" = "macOS" ]; then
        echo "  brew install node"
    else
        echo "  Ubuntu:  curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash - && sudo apt install -y nodejs"
    fi
    echo "  通用:    https://nodejs.org/zh-cn/download/"
    echo
    read -p "按回车退出..."
    exit 1
fi

NODE_VERSION=$(node -v)
echo -e "${CYAN}[信息]${RESET} Node.js 版本: $NODE_VERSION"

# --- 3. 检查 npm ---
if ! command -v npm &> /dev/null; then
    echo -e "${RED}[错误]${RESET} 未检测到 npm（应随 Node.js 一同安装）"
    read -p "按回车退出..."
    exit 1
fi

# --- 4. 检查 Rust / Cargo ---
if ! command -v cargo &> /dev/null; then
    echo -e "${RED}[错误]${RESET} 未检测到 cargo，Rust 工具链未安装"
    echo
    echo "请安装 Rust 1.80+："
    echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    echo "  source $HOME/.cargo/env"
    echo
    read -p "按回车退出..."
    exit 1
fi

RUST_VERSION=$(rustc --version 2>/dev/null)
echo -e "${CYAN}[信息]${RESET} Rust 版本: $RUST_VERSION"

# --- 5. 平台特定的系统依赖检查 ---
echo -e "${CYAN}[信息]${RESET} 检查系统依赖... ($PLATFORM_LABEL)"

if [ "$PLATFORM_LABEL" = "Linux" ]; then
    # === Linux 分支：检查 webkit2gtk / gtk3 等 ===
    MISSING_LIBS=()

    check_lib() {
        local lib=$1
        # 优先 ldconfig，回退到 find
        if ldconfig -p 2>/dev/null | grep -q "$lib"; then
            return 0
        fi
        if [ -f "/usr/lib/x86_64-linux-gnu/$lib" ] || [ -f "/usr/lib/$lib" ] || [ -f "/lib/x86_64-linux-gnu/$lib" ]; then
            return 0
        fi
        return 1
    }

    for lib in libwebkit2gtk-4.1.so.0 libgtk-3.so.0 libayatana-appindicator3.so.1 librsvg-2.so.2; do
        if ! check_lib "$lib"; then
            MISSING_LIBS+=("$lib")
        fi
    done

    if [ ${#MISSING_LIBS[@]} -ne 0 ]; then
        echo -e "${YELLOW}[警告]${RESET} 缺少以下系统库："
        for lib in "${MISSING_LIBS[@]}"; do
            echo "  - $lib"
        done
        echo
        echo "请安装系统依赖："
        echo "  Ubuntu/Debian: sudo apt install -y libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev patchelf"
        echo "  Fedora:        sudo dnf install -y webkit2gtk4.1-devel gtk3-devel libappindicator-gtk3-devel librsvg2-devel patchelf"
        echo "  Arch:          sudo pacman -S --needed webkit2gtk-4.1 gtk3 libappindicator-gtk3 librsvg patchelf"
        echo
        read -p "是否仍尝试打包？(y/N): " CONTINUE
        if [ "$CONTINUE" != "y" ] && [ "$CONTINUE" != "Y" ]; then
            exit 1
        fi
    else
        echo -e "${GREEN}[信息]${RESET} Linux 系统依赖检查通过"
    fi

elif [ "$PLATFORM_LABEL" = "macOS" ]; then
    # === macOS 分支：检查 Xcode Command Line Tools ===
    # Xcode CLT 是 macOS 上 Rust 编译的必需条件（提供 clang linker）
    # 缺失会导致 cargo 在链接阶段才失败，错误信息晦涩
    if ! xcode-select -p &> /dev/null; then
        echo -e "${RED}[错误]${RESET} 未检测到 Xcode Command Line Tools"
        echo
        echo "macOS 上 Rust 编译需要 Xcode CLT（提供 clang 编译器与 linker）。"
        echo "请执行以下命令安装："
        echo "  xcode-select --install"
        echo
        echo "安装完成后，重新运行本脚本。"
        echo
        echo "（如已安装但仍报错，执行：sudo xcode-select --reset）"
        echo
        read -p "按回车退出..."
        exit 1
    else
        XCODE_PATH=$(xcode-select -p 2>/dev/null)
        echo -e "${GREEN}[信息]${RESET} Xcode CLT 路径: $XCODE_PATH"
    fi

    # macOS 无需安装 Linux 的 webkit2gtk / gtk3 等库
    # WKWebView / NSStatusItem / Cocoa 都随 macOS SDK 内置
    echo -e "${GREEN}[信息]${RESET} macOS 系统依赖检查通过（WKWebView / Cocoa 由系统内置）"
fi

# --- 6. 首次运行：安装依赖 ---
if [ ! -d "node_modules" ]; then
    echo
    echo -e "${CYAN}[信息]${RESET} 首次运行，正在安装依赖（npm install）..."
    npm install
    if [ $? -ne 0 ]; then
        echo
        echo -e "${RED}[错误]${RESET} 依赖安装失败"
        echo "请检查网络或 npm 镜像配置后重试"
        read -p "按回车退出..."
        exit 1
    fi
fi

# --- 7. 显示打包选项菜单（平台特定） ---
echo
echo "------------------------------------------------------------"
echo "  请选择打包模式："
echo "------------------------------------------------------------"
echo "  [1] 完整打包（推荐发布，含测试）"
echo "  [2] 快速打包（跳过测试，开发迭代用）"
if [ "$PLATFORM_LABEL" = "Linux" ]; then
    echo "  [3] deb 安装包（Debian/Ubuntu）"
    echo "  [4] AppImage（便携运行，无需安装）"
elif [ "$PLATFORM_LABEL" = "macOS" ]; then
    echo "  [3] dmg 安装包（macOS 标准分发格式）"
    echo "  [4] .app 包（开发预览，不打包 dmg）"
fi
echo "  [5] 调试模式（不优化，仅验证编译）"
echo "  [0] 退出"
echo "------------------------------------------------------------"
read -p "请输入选项 [1/2/3/4/5/0]: " CHOICE

case "$CHOICE" in
    1) BUILD_ARGS="" ;;
    2) BUILD_ARGS="--skip-tests" ;;
    3)
        if [ "$PLATFORM_LABEL" = "Linux" ]; then
            BUILD_ARGS="--target deb --skip-tests"
        else
            BUILD_ARGS="--target dmg --skip-tests"
        fi
        ;;
    4)
        if [ "$PLATFORM_LABEL" = "Linux" ]; then
            BUILD_ARGS="--target appimage --skip-tests"
        else
            # macOS 的 .app 在 bundle/macos/ 下，dmg 内部已含 .app
            # 这里走默认打包（dmg），用户可从 dmg 内提取 .app
            echo -e "${YELLOW}[提示]${RESET} macOS 的 .app 包随 dmg 一起生成"
            echo "        生成后位于: src-tauri/target/release/bundle/macos/"
            echo "        将使用默认打包（dmg + .app）..."
            echo
            BUILD_ARGS="--skip-tests"
        fi
        ;;
    5) BUILD_ARGS="--debug --skip-tests" ;;
    0) exit 0 ;;
    *)
        echo -e "${RED}[错误]${RESET} 无效选项: $CHOICE"
        read -p "按回车退出..."
        exit 1
        ;;
esac

# --- 8. Build Jobs 全局配置（透传） ---
# 详见 build.config.README.md / pr/05-依赖与启动参数.md §5
# 优先级：环境变量 BUILD_JOBS > build.config.json > 自动检测 (上限 16)
# bash 默认会 export 环境变量到子进程，无需显式 export
echo
if [ -n "$BUILD_JOBS" ]; then
    echo -e "${CYAN}[信息]${RESET} BUILD_JOBS=$BUILD_JOBS (用户指定)"
else
    echo -e "${CYAN}[信息]${RESET} BUILD_JOBS=未设置（build.mjs 将自动检测或读取 build.config.json）"
fi

echo
echo -e "${CYAN}[信息]${RESET} 启动打包: node build.mjs $BUILD_ARGS"
echo

# --- 9. 调用 build.mjs ---
node build.mjs $BUILD_ARGS
EXIT_CODE=$?

echo
echo "============================================================"
if [ $EXIT_CODE -eq 0 ]; then
    echo -e "${GREEN}${BOLD}  ✅ 打包完成！${RESET}"
    echo
    echo "  产物位置: src-tauri/target/release/bundle/"
    echo "  清单文件: src-tauri/target/release/bundle/manifest.json"
else
    echo -e "${RED}${BOLD}  ❌ 打包失败（退出码 $EXIT_CODE）${RESET}"
    echo
    echo "  退出码含义："
    echo "    1 = 环境检查失败"
    echo "    2 = 类型检查失败"
    echo "    3 = 单元测试失败"
    echo "    4 = 构建失败"
    echo "    5 = 产物未找到"
    echo
    if [ "$PLATFORM_LABEL" = "macOS" ]; then
        echo "  macOS 常见故障："
        echo "    - linker 'cc' not found → xcode-select --install"
        echo "    - Gatekeeper 拦截 .dmg → 右键 → 打开 → 仍要打开"
        echo "    - is_comfyui_process 失效 → 已修复（详见 pr/08-验证清单.md §4.3.8）"
        echo
    fi
    echo "  故障排查请参考: pr/08-验证清单.md §4.3.8 (macOS) / §7 (Linux)"
fi
echo "============================================================"
echo

read -p "按回车退出..."
exit $EXIT_CODE
