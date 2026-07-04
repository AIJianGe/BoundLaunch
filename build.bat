@echo off
REM ============================================================================
REM  无界启动器 (BoundLaunch) Build Launcher (Windows)
REM
REM  Double-click to run. Delegates actual work to build.mjs.
REM  Uses goto pattern to avoid cmd parenthesized block parsing issues.
REM  See PR/08-verification-checklist.md section 4 for details.
REM ============================================================================

REM Switch to script directory (double-click may start in System32)
cd /d "%~dp0"

setlocal EnableDelayedExpansion

echo.
echo ============================================================
echo   无界启动器 Build Launcher (Windows)
echo   Backend script: build.mjs
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
echo.
echo Please install Node.js 18+ from:
echo   https://nodejs.org/en/download/
echo.
echo Or use nvm-windows: https://github.com/coreybutler/nvm-windows/releases
echo.
echo After installation, re-run this script.
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
echo.
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
echo.
echo Rust toolchain is not installed.
echo Please install Rust 1.80+ from:
echo   https://rustup.rs/
echo.
echo After installation, restart cmd and re-run this script.
echo.
pause
exit /b 1

:check_msvc
REM --- 4. Check Visual Studio Build Tools ---
where link.exe >nul 2>&1
if errorlevel 1 goto :msvc_locate
echo [INFO] MSVC link.exe found in PATH
goto :check_node_modules

:msvc_locate
REM Try to locate MSVC via vswhere.exe (VS Installer ships it)
set VSWHERE_PATH=C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe
if not exist "%VSWHERE_PATH%" goto :msvc_warn

set VS_INSTALL_PATH=
for /f "usebackq tokens=*" %%p in (`"%VSWHERE_PATH%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set VS_INSTALL_PATH=%%p

if "%VS_INSTALL_PATH%"=="" goto :msvc_warn

set VCVARS_PATH=%VS_INSTALL_PATH%\VC\Auxiliary\Build\vcvars64.bat
if not exist "%VCVARS_PATH%" goto :msvc_warn

echo [INFO] Loading MSVC environment: %VCVARS_PATH%
call "%VCVARS_PATH%" >nul 2>&1

REM Re-check link.exe after loading environment
where link.exe >nul 2>&1
if errorlevel 1 goto :msvc_warn
echo [INFO] MSVC link.exe found via VS %VS_INSTALL_PATH%
goto :check_node_modules

:msvc_warn
echo [WARN] MSVC link.exe not found
echo.
echo Windows Rust compilation requires Visual Studio Build Tools 2022
echo with the "Desktop development with C++" workload.
echo   https://visualstudio.microsoft.com/visual-cpp-build-tools/
echo.
echo Build will likely fail if not installed.
echo.
choice /c YN /m "Continue anyway"
if errorlevel 2 exit /b 1
goto :check_node_modules

:check_node_modules
REM --- 5. First run: install dependencies ---
if not exist "node_modules" goto :install_deps
goto :show_menu

:install_deps
echo.
echo [INFO] First run, installing dependencies - npm install ...
call npm install
if errorlevel 1 goto :install_failed
echo [INFO] Dependencies installed successfully

:show_menu
REM --- 6. Show build mode menu ---
echo.
echo ------------------------------------------------------------
echo  Select build mode:
echo ------------------------------------------------------------
echo   [1] Full build - recommended for release, with tests
echo   [2] Fast build - skip tests, for dev iteration
echo   [3] NSIS installer - .exe, Windows only
echo   [4] MSI installer - for enterprise deployment
echo   [5] Debug mode - no optimization, compile check only
echo   [0] Exit
echo ------------------------------------------------------------
set /p CHOICE=Enter choice [1/2/3/4/5/0]:

set BUILD_ARGS=
if "%CHOICE%"=="1" goto :choice_1
if "%CHOICE%"=="2" goto :choice_2
if "%CHOICE%"=="3" goto :choice_3
if "%CHOICE%"=="4" goto :choice_4
if "%CHOICE%"=="5" goto :choice_5
if "%CHOICE%"=="0" goto :choice_exit
echo [ERROR] Invalid choice: %CHOICE%
pause
exit /b 1

:choice_1
set BUILD_ARGS=
goto :start_build

:choice_2
set BUILD_ARGS=--skip-tests
goto :start_build

:choice_3
set BUILD_ARGS=--target nsis --skip-tests
goto :start_build

:choice_4
set BUILD_ARGS=--target msi --skip-tests
goto :start_build

:choice_5
set BUILD_ARGS=--debug --skip-tests
goto :start_build

:choice_exit
exit /b 0

:start_build
echo.
echo [INFO] Starting build: node build.mjs %BUILD_ARGS%
echo.

REM --- 6.5 Build Jobs 全局配置（透传） ---
REM 详见 build.config.README.md / pr\05-依赖与启动参数.md §5
REM 优先级：环境变量 BUILD_JOBS > build.config.json > 自动检测 (上限 16)
if defined BUILD_JOBS (
  echo [INFO] BUILD_JOBS=%BUILD_JOBS% (用户指定)
) else (
  echo [INFO] BUILD_JOBS=未设置（build.mjs 将自动检测或读取 build.config.json）
)

REM Switch console to UTF-8 so build.mjs Chinese output displays correctly
chcp 65001 >nul 2>&1

REM --- 7. Call build.mjs ---
REM Windows CMD 默认会透传环境变量到子进程，无需显式 export
node build.mjs %BUILD_ARGS%
set EXIT_CODE=%ERRORLEVEL%
goto :report

:install_failed
echo.
echo [ERROR] Dependency installation failed
echo Please check network or npm mirror configuration
echo.
echo Tip: You can use a China mirror to speed up npm install:
echo   npm config set registry https://registry.npmmirror.com
echo.
pause
exit /b 1

:report
echo.
echo ============================================================
if %EXIT_CODE% equ 0 goto :report_success
goto :report_failure

:report_success
echo   BUILD SUCCESS
echo.
echo   Artifacts location: src-tauri\target\release\bundle\
echo   Manifest file:      src-tauri\target\release\bundle\manifest.json
echo ============================================================
echo.
pause
exit /b 0

:report_failure
echo   BUILD FAILED - exit code %EXIT_CODE%
echo.
echo   Exit code meaning:
echo     1 = Environment check failed
echo     2 = Type check failed
echo     3 = Unit tests failed
echo     4 = Build failed
echo     5 = Artifacts not found
echo.
echo   See PR\08-verification-checklist.md section 7 for troubleshooting
echo ============================================================
echo.
pause
exit /b %EXIT_CODE%
