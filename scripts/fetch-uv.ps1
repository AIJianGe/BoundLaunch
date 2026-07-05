# ============================================================================
# 下载 uv 二进制到 src-tauri/resources/uv/（sidecar 资源）
#
# 使用方法（仓库根目录运行）：
#   .\scripts\fetch-uv.ps1            # 下载本机平台
#   .\scripts\fetch-uv.ps1 -All       # 下载所有平台（4 个）
#
# 前置条件：
#   - PowerShell 5+（Windows 自带）
#   - Invoke-WebRequest（系统自带）
#   - Expand-Archive（系统自带）
# ============================================================================

[CmdletBinding()]
param(
    [switch]$All,
    [string]$Version = "0.4.18"
)

$ErrorActionPreference = "Stop"

# 切换到仓库根目录
# 注：优先用 $PSScriptRoot（PS 3.0+ 自动变量，最可靠）；
# 回退到 $MyInvocation.MyCommand.Definition（兼容老版本，但在 & {} 包裹下可能为空）
$ScriptDir = if ($PSScriptRoot) {
    $PSScriptRoot
} elseif ($MyInvocation.MyCommand.Definition) {
    Split-Path -Parent $MyInvocation.MyCommand.Definition
} else {
    # 最后兜底：假设当前工作目录就是仓库根目录
    (Get-Location).Path
}
$RepoRoot = Split-Path -Parent $ScriptDir
Set-Location $RepoRoot

Write-Host "============================================================" -ForegroundColor Cyan
Write-Host "  uv Sidecar Fetcher (version $Version)" -ForegroundColor Cyan
Write-Host "============================================================" -ForegroundColor Cyan
Write-Host ""

# 检查目标目录
$TargetDir = Join-Path $RepoRoot "src-tauri\resources\uv"
if (-not (Test-Path $TargetDir)) {
    New-Item -ItemType Directory -Path $TargetDir -Force | Out-Null
    Write-Host "[OK] Created $TargetDir" -ForegroundColor Green
}

# 平台定义
$Platforms = @(
    @{ Name = "Windows x86_64";     Triple = "x86_64-pc-windows-msvc";     Asset = "uv-x86_64-pc-windows-msvc.zip";     IsZip = $true  }
    @{ Name = "macOS Intel";        Triple = "x86_64-apple-darwin";        Asset = "uv-x86_64-apple-darwin.tar.gz";      IsZip = $false }
    @{ Name = "macOS Apple Silicon";Triple = "aarch64-apple-darwin";       Asset = "uv-aarch64-apple-darwin.tar.gz";     IsZip = $false }
    @{ Name = "Linux x86_64";       Triple = "x86_64-unknown-linux-gnu";   Asset = "uv-x86_64-unknown-linux-gnu.tar.gz"; IsZip = $false }
)

# 决定下载哪些
if ($All) {
    $Selected = $Platforms
    Write-Host "[INFO] Downloading all platforms ($($Platforms.Count))" -ForegroundColor Yellow
} else {
    # 通过 rustc 检测本机 triple
    $RustcTriple = $null
    try {
        $RustcOut = & rustc -vV 2>$null
        $Line = $RustcOut | Where-Object { $_ -match '^host:' } | Select-Object -First 1
        if ($Line) {
            $RustcTriple = ($Line -split ':\s*')[1].Trim()
        }
    } catch {}

    if (-not $RustcTriple) {
        Write-Host "[WARN] Cannot detect host triple via rustc, falling back to current platform" -ForegroundColor Yellow
        if ($IsWindows) { $RustcTriple = "x86_64-pc-windows-msvc" }
        elseif ($IsMacOS) {
            $Arch = (uname -m)
            if ($Arch -eq "arm64") { $RustcTriple = "aarch64-apple-darwin" }
            else { $RustcTriple = "x86_64-apple-darwin" }
        } else { $RustcTriple = "x86_64-unknown-linux-gnu" }
    }

    Write-Host "[INFO] Detected host triple: $RustcTriple" -ForegroundColor Yellow
    $Selected = $Platforms | Where-Object { $_.Triple -eq $RustcTriple }
    if (-not $Selected) {
        Write-Host "[ERROR] Unsupported platform: $RustcTriple" -ForegroundColor Red
        Write-Host "Use -All to download all platforms" -ForegroundColor Yellow
        exit 1
    }
}

# 下载并解压
$TempDir = Join-Path $env:TEMP "uv-fetch-$([guid]::NewGuid().ToString('N').Substring(0,8))"
New-Item -ItemType Directory -Path $TempDir -Force | Out-Null

try {
    foreach ($P in $Selected) {
        $Url = "https://github.com/astral-sh/uv/releases/download/$Version/$($P.Asset)"
        $TargetFile = Join-Path $TargetDir ("uv-" + $P.Triple + ($(if ($IsWindows) { ".exe" } else { "" })))
        $DownloadPath = Join-Path $TempDir $P.Asset

        Write-Host ""
        Write-Host "[$($P.Name)] Downloading $Url ..." -ForegroundColor Cyan

        try {
            Invoke-WebRequest -Uri $Url -OutFile $DownloadPath -UseBasicParsing -ErrorAction Stop
        } catch {
            Write-Host "[ERROR] Download failed: $_" -ForegroundColor Red
            Write-Host "Hint: GitHub may be slow/unreachable in China. Use a mirror:" -ForegroundColor Yellow
            Write-Host "  https://ghfast.top/https://github.com/astral-sh/uv/releases/download/$Version/$($P.Asset)" -ForegroundColor Yellow
            continue
        }

        Write-Host "[$($P.Name)] Extracting ..." -ForegroundColor Cyan

        $ExtractDir = Join-Path $TempDir ([guid]::NewGuid().ToString('N').Substring(0,8))
        New-Item -ItemType Directory -Path $ExtractDir -Force | Out-Null

        if ($P.IsZip) {
            Expand-Archive -Path $DownloadPath -DestinationPath $ExtractDir -Force
        } else {
            # tar.gz via tar (Windows 10+ / Linux / macOS 都自带)
            & tar -xzf $DownloadPath -C $ExtractDir
        }

        # 找 uv 二进制
        $UvBinary = Get-ChildItem -Path $ExtractDir -Recurse -Filter "uv*" -File | Where-Object { $_.Name -eq "uv" -or $_.Name -eq "uv.exe" } | Select-Object -First 1
        if (-not $UvBinary) {
            Write-Host "[ERROR] uv binary not found in archive" -ForegroundColor Red
            continue
        }

        Copy-Item -Path $UvBinary.FullName -Destination $TargetFile -Force

        # Unix: chmod +x
        if (-not $IsWindows) {
            & chmod +x $TargetFile
        }

        $SizeKB = [math]::Round((Get-Item $TargetFile).Length / 1KB, 1)
        Write-Host "[$($P.Name)] OK -> $TargetFile ($SizeKB KB)" -ForegroundColor Green
    }
} finally {
    Remove-Item -Recurse -Force $TempDir -ErrorAction SilentlyContinue
}

Write-Host ""
Write-Host "============================================================" -ForegroundColor Cyan
Write-Host "  Done!" -ForegroundColor Green
Write-Host "  Files in: $TargetDir" -ForegroundColor Cyan
Write-Host ""
Get-ChildItem $TargetDir -File | Where-Object { $_.Name -ne ".gitkeep" -and $_.Name -ne "README.md" } | ForEach-Object {
    Write-Host "    - $($_.Name) ($([math]::Round($_.Length/1KB,1)) KB)" -ForegroundColor Gray
}
Write-Host ""
Write-Host "Next: re-run 'npm run tauri dev' to use sidecar uv" -ForegroundColor Yellow
Write-Host "============================================================" -ForegroundColor Cyan
