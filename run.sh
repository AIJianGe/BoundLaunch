#!/usr/bin/env bash
# ============================================================================
#  无界启动器 (BoundLaunch) Dev Runner (Linux / macOS)
#
#  Usage:
#    chmod +x run.sh
#    ./run.sh
#
#  Launches `npm run tauri dev` directly (no packaging).
#  - Frontend: Vite dev server with HMR (http://localhost:5173)
#  - Backend:  Rust hot-reload via Tauri
#  - Window:   Tauri opens app window automatically
#
#  First run triggers full Rust compile (~5-10 min). Subsequent runs are fast.
#  Press Ctrl+C in this terminal to stop the app.
# ============================================================================

set -e

# Switch to script directory
cd "$(dirname "$0")"

# ANSI color codes
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

info()  { echo -e "${BLUE}[INFO]${NC}  $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*"; }
ok()    { echo -e "${GREEN}[OK]${NC}    $*"; }

echo ""
echo "============================================================"
echo "  无界启动器 Dev Runner (Linux / macOS)"
echo "  Command: npm run tauri dev"
echo "============================================================"
echo ""

# Detect OS
OS_NAME="$(uname -s)"
case "$OS_NAME" in
  Linux*)  PLATFORM="Linux" ;;
  Darwin*) PLATFORM="macOS" ;;
  *)       PLATFORM="Unknown ($OS_NAME)" ;;
esac
info "Detected platform: $PLATFORM"

# --- 1. Check Node.js ---
if ! command -v node >/dev/null 2>&1; then
  error "Node.js not found in PATH"
  echo ""
  echo "Please install Node.js 18+:"
  echo "  https://nodejs.org/en/download/"
  echo ""
  echo "Or use nvm:"
  echo "  curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.39.7/install.sh | bash"
  echo "  nvm install 18"
  exit 1
fi
NODE_VERSION="$(node -v)"
info "Node.js version: $NODE_VERSION"

# --- 2. Check npm ---
if ! command -v npm >/dev/null 2>&1; then
  error "npm not found in PATH"
  echo "npm should be installed together with Node.js."
  exit 1
fi
NPM_VERSION="$(npm -v)"
info "npm version: $NPM_VERSION"

# --- 3. Check Rust / Cargo ---
if ! command -v cargo >/dev/null 2>&1; then
  error "cargo / rustc not found in PATH"
  echo ""
  echo "Please install Rust 1.80+:"
  echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
  exit 1
fi
RUST_VERSION="$(rustc --version)"
info "Rust version: $RUST_VERSION"

# --- 4. Platform-specific system dependency check ---
check_linux_deps() {
  # Tauri 2 system dependencies for Linux
  local missing=()
  for pkg in libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev; do
    if ! dpkg -s "$pkg" >/dev/null 2>&1; then
      missing+=("$pkg")
    fi
  done
  if [ ${#missing[@]} -gt 0 ]; then
    warn "Missing Linux system dependencies: ${missing[*]}"
    echo ""
    echo "Install them with:"
    echo "  sudo apt install -y ${missing[*]} libayatana-appindicator3-dev patchelf"
    echo ""
    echo "For Fedora:"
    echo "  sudo dnf install -y webkit2gtk4.1-devel gtk3-devel libappindicator-gtk3-devel librsvg2-devel patchelf"
    echo ""
    read -p "Continue anyway? [y/N] " -n 1 -r
    echo ""
    [[ ! $REPLY =~ ^[Yy]$ ]] && exit 1
  fi
}

check_macos_deps() {
  # Xcode Command Line Tools
  if ! xcode-select -p >/dev/null 2>&1; then
    warn "Xcode Command Line Tools not found"
    echo ""
    echo "Install them with:"
    echo "  xcode-select --install"
    echo ""
    read -p "Continue anyway? [y/N] " -n 1 -r
    echo ""
    [[ ! $REPLY =~ ^[Yy]$ ]] && exit 1
  fi
}

case "$OS_NAME" in
  Linux*)  check_linux_deps ;;
  Darwin*) check_macos_deps ;;
esac

# --- 5. First run: install dependencies ---
if [ ! -d "node_modules" ]; then
  echo ""
  info "First run, installing dependencies - npm install ..."
  npm install
  ok "Dependencies installed successfully"
fi

# --- 6. Launch tauri dev ---
echo ""
echo "============================================================"
echo "  Starting dev mode..."
echo "  - Frontend HMR: http://localhost:5173"
echo "  - Tauri window will open automatically"
echo "  - First compile takes ~5-10 min (Rust), subsequent runs are fast"
echo "  - Press Ctrl+C in this terminal to stop"
echo "============================================================"
echo ""

npm run tauri dev
EXIT_CODE=$?

echo ""
if [ $EXIT_CODE -eq 0 ]; then
  ok "Dev session ended normally."
else
  error "Dev session ended with exit code $EXIT_CODE."
  echo "See PR/08-verification-checklist.md section 7 for troubleshooting."
fi
exit $EXIT_CODE
