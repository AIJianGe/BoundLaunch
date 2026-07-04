#!/bin/bash
# ============================================================================
#  myComfyUI 打包启动器 (Linux)
#
#  双击运行说明：
#    1. 首次使用：右键 build.sh → 属性 → 允许作为程序执行
#       或在终端执行：chmod +x build.sh
#    2. 在文件管理器中双击 build.sh
#       - GNOME: 首次需右键 → "Allow Launching"
#       - KDE: 默认双击可执行
#       - XFCE: 默认双击可执行
#    3. 若双击打开文本编辑器，请用终端执行：./build.sh
#
#  实际工作交给 build.mjs，本脚本仅负责环境检查 + 调用。
#  详见 PR/08-验证清单.md §4 跨平台打包
# ============================================================================

# 切换到脚本所在目录（双击时工作目录可能是 ~）
cd "$(dirname "$0")" || exit 1

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
echo -e "${BOLD}${CYAN}  myComfyUI 打包启动器 (Linux)${RESET}"
echo "  实际工作脚本: build.mjs"
echo "============================================================"
echo

# --- 0. 检查脚本执行权限（自动 chmod）---
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

# --- 1. 检查 Node.js ---
if ! command -v node &> /dev/null; then
    echo -e "${RED}[错误]${RESET} 未检测到 Node.js"
    echo
    echo "请先安装 Node.js 18+："
    echo "  Ubuntu:  curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash - && sudo apt install -y nodejs"
    echo "  通用:    https://nodejs.org/zh-cn/download/"
    echo
    read -p "按回车退出..."
    exit 1
fi

NODE_VERSION=$(node -v)
echo -e "${CYAN}[信息]${RESET} Node.js 版本: $NODE_VERSION"

# --- 2. 检查 npm ---
if ! command -v npm &> /dev/null; then
    echo -e "${RED}[错误]${RESET} 未检测到 npm（应随 Node.js 一同安装）"
    read -p "按回车退出..."
    exit 1
fi

# --- 3. 检查 Rust / Cargo ---
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

# --- 4. 检查系统依赖（Linux 必需的库）---
echo -e "${CYAN}[信息]${RESET} 检查系统依赖..."
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
    echo -e "${GREEN}[信息]${RESET} 系统依赖检查通过"
fi

# --- 5. 首次运行：安装依赖 ---
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

# --- 6. 显示打包选项菜单 ---
echo
echo "------------------------------------------------------------"
echo "  请选择打包模式："
echo "------------------------------------------------------------"
echo "  [1] 完整打包（推荐发布，含测试）"
echo "  [2] 快速打包（跳过测试，开发迭代用）"
echo "  [3] deb 安装包（Debian/Ubuntu）"
echo "  [4] AppImage（便携运行，无需安装）"
echo "  [5] 调试模式（不优化，仅验证编译）"
echo "  [0] 退出"
echo "------------------------------------------------------------"
read -p "请输入选项 [1/2/3/4/5/0]: " CHOICE

case "$CHOICE" in
    1) BUILD_ARGS="" ;;
    2) BUILD_ARGS="--skip-tests" ;;
    3) BUILD_ARGS="--target deb --skip-tests" ;;
    4) BUILD_ARGS="--target appimage --skip-tests" ;;
    5) BUILD_ARGS="--debug --skip-tests" ;;
    0) exit 0 ;;
    *)
        echo -e "${RED}[错误]${RESET} 无效选项: $CHOICE"
        read -p "按回车退出..."
        exit 1
        ;;
esac

echo
echo -e "${CYAN}[信息]${RESET} 启动打包: node build.mjs $BUILD_ARGS"
echo

# --- 7. 调用 build.mjs ---
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
    echo "  故障排查请参考: PR/08-验证清单.md §7"
fi
echo "============================================================"
echo

read -p "按回车退出..."
exit $EXIT_CODE
