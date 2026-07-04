@echo off
REM ============================================================================
REM  无界启动器 (BoundLaunch) Dev Runner (Windows)
REM
REM  Double-click to run. Launches `npm run tauri dev` directly (no packaging).
REM  - Frontend: Vite dev server with HMR (http://localhost:5173)
REM  - Backend:  Rust hot-reload via Tauri
REM  - Window:   Tauri opens app window automatically
REM
REM  First run triggers full Rust compile (~5-10 min). Subsequent runs are fast.
REM  Press Ctrl+C in this window to stop the app.
REM ============================================================================

REM Switch to script directory (double-click may start in System32)
cd /d "%~dp0"

setlocal EnableDelayedExpansion

echo.
echo ============================================================
echo   无界启动器 Dev Runner (Windows)
echo   Command: npm run tauri dev
echo ============================================================
echo.

REM --- 1. Check Node.js ---
where node >nul 2>&1
if errorlevel 1 goto :node_not_found

for /f "tokens=*" %%v in ('node -v') do set NODE_VERSION=%%v
echo [INFO] Node.js version: %NODE_VERSION%
goto :check_npm

:node_not_found
echo [ERROR] Node.js not found in PATH
echo Please install Node.js 18+ from: https://nodejs.org/en/download/
echo Or use nvm-windows: https://github.com/coreybutler/nvm-windows/releases
echo.
pause
exit /b 1

:check_npm
REM --- 2. Check npm ---
where npm >nul 2>&1
if errorlevel 1 goto :npm_not_found

for /f "tokens=*" %%v in ('npm -v') do set NPM_VERSION=%%v
echo [INFO] npm version: %NPM_VERSION%
goto :check_rust

:npm_not_found
echo [ERROR] npm not found in PATH
echo npm should be installed together with Node.js.
echo If you used nvm-windows, please run: nvm use X.Y.Z
echo.
pause
exit /b 1

:check_rust
REM --- 3. Check Rust / Cargo ---
where cargo >nul 2>&1
if errorlevel 1 goto :rust_not_found

for /f "tokens=*" %%v in ('rustc --version') do set RUST_VERSION=%%v
echo [INFO] Rust version: %RUST_VERSION%
goto :check_msvc

:rust_not_found
echo [ERROR] cargo / rustc not found in PATH
echo Please install Rust 1.80+ from: https://rustup.rs/
echo.
pause
exit /b 1

:check_msvc
REM --- 4. Check Visual Studio Build Tools (Windows Rust compilation requires it) ---
where link.exe >nul 2>&1
if errorlevel 1 goto :msvc_locate
echo [INFO] MSVC link.exe found in PATH
goto :check_node_modules

:msvc_locate
set VSWHERE_PATH=C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe
if not exist "%VSWHERE_PATH%" goto :msvc_warn

set VS_INSTALL_PATH=
for /f "usebackq tokens=*" %%p in (`"%VSWHERE_PATH%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set VS_INSTALL_PATH=%%p

if "%VS_INSTALL_PATH%"=="" goto :msvc_warn

set VCVARS_PATH=%VS_INSTALL_PATH%\VC\Auxiliary\Build\vcvars64.bat
if not exist "%VCVARS_PATH%" goto :msvc_warn

echo [INFO] Loading MSVC environment: %VCVARS_PATH%
call "%VCVARS_PATH%" >nul 2>&1

where link.exe >nul 2>&1
if errorlevel 1 goto :msvc_warn
echo [INFO] MSVC link.exe found via VS %VS_INSTALL_PATH%
goto :check_node_modules

:msvc_warn
echo [WARN] MSVC link.exe not found
echo Windows Rust compilation requires Visual Studio Build Tools 2022
echo with the "Desktop development with C++" workload.
echo   https://visualstudio.microsoft.com/visual-cpp-build-tools/
echo.
choice /c YN /m "Continue anyway"
if errorlevel 2 exit /b 1
goto :check_node_modules

:check_node_modules
REM --- 5. First run: install dependencies ---
if exist "node_modules" goto :start_dev

echo.
echo [INFO] First run, installing dependencies - npm install ...
call npm install
if errorlevel 1 goto :install_failed
echo [INFO] Dependencies installed successfully

:start_dev
echo.
echo ============================================================
echo  Starting dev mode...
echo  - Frontend HMR: http://localhost:5173
echo  - Tauri window will open automatically
echo  - First compile takes ~5-10 min (Rust), subsequent runs are fast
echo  - Press Ctrl+C in this window to stop
echo ============================================================
echo.

REM Switch console to UTF-8 so Tauri/Vite Chinese output displays correctly
chcp 65001 >nul 2>&1

REM --- 6. Launch tauri dev directly ---
call npm run tauri dev
set EXIT_CODE=%ERRORLEVEL%

echo.
if %EXIT_CODE% equ 0 (
  echo   Dev session ended normally.
) else (
  echo   Dev session ended with exit code %EXIT_CODE%.
  echo   See PR\08-verification-checklist.md section 7 for troubleshooting.
)
echo.
pause
exit /b %EXIT_CODE%

:install_failed
echo.
echo [ERROR] Dependency installation failed
echo Please check network or npm mirror configuration.
echo Tip: Use China mirror to speed up npm install:
echo   npm config set registry https://registry.npmmirror.com
echo.
pause
exit /b 1