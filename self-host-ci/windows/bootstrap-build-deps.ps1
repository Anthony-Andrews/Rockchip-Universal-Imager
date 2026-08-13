#Requires -Version 5.1
<#
.SYNOPSIS
  Bootstrap Windows build-server deps for Rockchip Universal Imager + rkdeveloptool.

.DESCRIPTION
  Run in an elevated PowerShell (Run as Administrator) on the Windows CI host.
  Self-hosted selection: [self-hosted, Windows, X64] (any hostname, e.g. Windows-11)

    Set-ExecutionPolicy -Scope Process Bypass -Force
    .\self-host-ci\windows\bootstrap-build-deps.ps1
    .\self-host-ci\windows\bootstrap-build-deps.ps1 -SkipTauriCli
    .\self-host-ci\windows\bootstrap-build-deps.ps1 -SkipLlvmMingw
    .\self-host-ci\windows\bootstrap-build-deps.ps1 -SkipVsBuildTools

  ---------------------------------------------------------------------------
  Runner expectations (workflows assume these are pre-installed)
  ---------------------------------------------------------------------------
  Used by:
    .github/workflows/build-rkdeveloptool.yaml  (windows-x86_64, windows-aarch64)
    .github/workflows/portable.yml / installer.yml

  Required for rkdeveloptool (autogen + configure + make via MinGW):
    - Git for Windows (bash available to Actions)
    - MSYS2 at C:\msys64 with MINGW64:
        mingw-w64-x86_64-gcc / g++
        mingw-w64-x86_64-libusb   (libusb-1.0.dll + libusb-1.0.a + headers)
        mingw-w64-x86_64-pkg-config
        autoconf, automake, libtool, m4, make
        zip, unzip, tar, bzip2, curl/wget, git
    - llvm-mingw at C:\llvm-mingw (aarch64-w64-mingw32-gcc/g++) for arm64
      (CI may also build libusb from source for windows-aarch64)

  Required for Tauri app (MSVC):
    - Visual Studio 2022 Build Tools (x64 + ARM64 toolsets) + Windows SDK
    - CMake, Ninja
    - rustup + stable, targets x86_64-pc-windows-msvc + aarch64-pc-windows-msvc
    - tauri-cli ^2 (optional: -SkipTauriCli)

  Installs all of the above.

.NOTES
  Prefer winget. Falls back to direct downloads where needed.
  libusb is libusb-1.0 (MinGW), not legacy libusb-win32 0.1.
#>
[CmdletBinding()]
param(
    [switch]$SkipTauriCli,
    [switch]$SkipLlvmMingw,
    [switch]$SkipVsBuildTools,
    [switch]$SkipWingetTools
)

$ErrorActionPreference = "Stop"

function Write-Step([string]$Message) {
    Write-Host ""
    Write-Host "==> $Message" -ForegroundColor Cyan
}

function Test-Admin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    $p = New-Object Security.Principal.WindowsPrincipal($id)
    return $p.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Refresh-Path {
    $machine = [Environment]::GetEnvironmentVariable("Path", "Machine")
    $user = [Environment]::GetEnvironmentVariable("Path", "User")
    $env:Path = "$machine;$user"
    if (Test-Path "$env:USERPROFILE\.cargo\bin") {
        $env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
    }
    if (Test-Path "C:\msys64\usr\bin") {
        $env:Path = "C:\msys64\usr\bin;C:\msys64\mingw64\bin;$env:Path"
    }
    if (Test-Path "C:\llvm-mingw\bin") {
        $env:Path = "C:\llvm-mingw\bin;$env:Path"
    }
}

function Assert-Command([string]$Name) {
    return [bool](Get-Command $Name -ErrorAction SilentlyContinue)
}

function Invoke-WingetInstall([string]$Id, [string]$Name) {
    if (-not (Assert-Command "winget")) {
        Write-Warning "winget not found; skip $Name ($Id)"
        return $false
    }
    Write-Step "winget install $Name ($Id)"
    & winget install --id $Id -e --accept-package-agreements --accept-source-agreements --disable-interactivity
    if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne -1978335189) {
        # -1978335189 often means already installed
        Write-Warning "winget returned $LASTEXITCODE for $Id (may already be installed)"
    }
    Refresh-Path
    return $true
}

# ---------------------------------------------------------------------------
# Prerequisites
# ---------------------------------------------------------------------------
if (-not (Test-Admin)) {
    Write-Error "Run this script in an elevated PowerShell (Run as Administrator)."
}

Write-Step "Windows build-dep bootstrap (user=$env:USERNAME)"

# ---------------------------------------------------------------------------
# Core tools via winget
# ---------------------------------------------------------------------------
if (-not $SkipWingetTools) {
    if (Assert-Command "winget") {
        Invoke-WingetInstall "Git.Git" "Git for Windows" | Out-Null
        Invoke-WingetInstall "Kitware.CMake" "CMake" | Out-Null
        Invoke-WingetInstall "Ninja-build.Ninja" "Ninja" | Out-Null
        Invoke-WingetInstall "7zip.7zip" "7-Zip" | Out-Null
        # NSIS for package.yaml Windows installers (makensis)
        Invoke-WingetInstall "NSIS.NSIS" "NSIS" | Out-Null
    } else {
        Write-Warning "winget missing. Install App Installer from Microsoft Store, or install Git/CMake/Ninja manually."
    }
} else {
    Write-Step "Skipping winget tools (-SkipWingetTools)"
}

# ---------------------------------------------------------------------------
# Visual Studio 2022 Build Tools (MSVC x64 + ARM64)
# ---------------------------------------------------------------------------
if (-not $SkipVsBuildTools) {
    Write-Step "Visual Studio 2022 Build Tools (MSVC + ARM64)"
    if (Assert-Command "winget") {
        # Workloads: MSVC tools, Windows SDK. ARM64 components via extra override.
        $vsId = "Microsoft.VisualStudio.2022.BuildTools"
        $override = @(
            "--wait"
            "--passive"
            "--add", "Microsoft.VisualStudio.Workload.VCTools"
            "--add", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64"
            "--add", "Microsoft.VisualStudio.Component.VC.Tools.ARM64"
            "--add", "Microsoft.VisualStudio.Component.Windows11SDK.22621"
            "--includeRecommended"
        ) -join " "
        & winget install --id $vsId -e --accept-package-agreements --accept-source-agreements --disable-interactivity --override $override
        if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne -1978335189) {
            Write-Warning "VS Build Tools winget exit $LASTEXITCODE - install/repair via Visual Studio Installer if needed."
        }
    } else {
        Write-Warning "Install VS 2022 Build Tools manually with Desktop C++ and ARM64 tools."
    }
} else {
    Write-Step "Skipping VS Build Tools (-SkipVsBuildTools)"
}

# ---------------------------------------------------------------------------
# MSYS2 + MinGW64 packages
# ---------------------------------------------------------------------------
$MsysRoot = "C:\msys64"
$Bash = Join-Path $MsysRoot "usr\bin\bash.exe"
$Pacman = Join-Path $MsysRoot "usr\bin\pacman.exe"

function Install-Msys2 {
    if (Test-Path $Bash) {
        Write-Step "MSYS2 already present at $MsysRoot"
        return
    }
    Write-Step "Installing MSYS2 to $MsysRoot"
    if (Assert-Command "winget") {
        Invoke-WingetInstall "MSYS2.MSYS2" "MSYS2" | Out-Null
    }
    if (-not (Test-Path $Bash)) {
        $installer = Join-Path $env:TEMP "msys2-x86_64-latest.exe"
        Write-Step "Downloading MSYS2 installer..."
        Invoke-WebRequest -Uri "https://github.com/msys2/msys2-installer/releases/latest/download/msys2-x86_64-latest.exe" -OutFile $installer
        Write-Step "Running MSYS2 unattended install..."
        Start-Process -FilePath $installer -ArgumentList @("install", "--root", $MsysRoot, "--confirm-command") -Wait
    }
    if (-not (Test-Path $Bash)) {
        throw "MSYS2 bash not found at $Bash after install."
    }
}

function Invoke-Msys([string]$Command) {
    if (-not (Test-Path $Bash)) { throw "MSYS2 bash missing: $Bash" }
    # Login-ish env without interactive profile noise
    & $Bash -lc $Command
    if ($LASTEXITCODE -ne 0) {
        throw "MSYS2 command failed ($LASTEXITCODE): $Command"
    }
}

Install-Msys2
Refresh-Path

# Machine env so CI discovers tools without hardcoding drive letters / OneDrive layouts
Write-Step "Setting machine env MSYS2_ROOT / MSYS2_BASH (install-location independent for CI)"
[Environment]::SetEnvironmentVariable("MSYS2_ROOT", $MsysRoot, "Machine")
[Environment]::SetEnvironmentVariable("MSYS2_BASH", $Bash, "Machine")
$env:MSYS2_ROOT = $MsysRoot
$env:MSYS2_BASH = $Bash

Write-Step "MSYS2 pacman: update + MinGW64 toolchain / libusb / autotools"
# First-time keyring / update can need multiple passes
try {
    Invoke-Msys "pacman -Syu --noconfirm"
} catch {
    Write-Warning "First pacman -Syu may require re-run after MSYS2 core update: $_"
    Invoke-Msys "pacman -Syu --noconfirm"
}

# MSYS2 packages for rkdeveloptool + staging libusb-1.0.dll (libusb-1.0, not libusb-win32)
$mingwPkgs = @(
    # MinGW64 C++ toolchain (rkdeveloptool x86_64)
    "mingw-w64-x86_64-gcc"
    "mingw-w64-x86_64-libusb"
    "mingw-w64-x86_64-pkg-config"
    # autotools (./autogen.sh -> autoreconf)
    "autoconf"
    "automake"
    "libtool"
    "m4"
    "make"
    "patch"
    # packaging + libusb source fetch (curl + bzip2 for .tar.bz2)
    "zip"
    "unzip"
    "tar"
    "bzip2"
    "curl"
    "wget"
    "git"
    "ca-certificates"
) -join " "

Invoke-Msys "pacman -S --needed --noconfirm $mingwPkgs"

# ---------------------------------------------------------------------------
# llvm-mingw (Windows aarch64 cross for rkdeveloptool)
# ---------------------------------------------------------------------------
$LlvmMingwRoot = "C:\llvm-mingw"
if (-not $SkipLlvmMingw) {
    $clangAarch = Join-Path $LlvmMingwRoot "bin\aarch64-w64-mingw32-gcc.exe"
    if (Test-Path $clangAarch) {
        Write-Step "llvm-mingw already present at $LlvmMingwRoot"
    } else {
        Write-Step "Installing llvm-mingw to $LlvmMingwRoot"
        # Pin to a known release; update URL if the project moves tags.
        $llvmUrl = "https://github.com/mstorsjo/llvm-mingw/releases/download/20240606/llvm-mingw-20240606-ucrt-x86_64.zip"
        $zipPath = Join-Path $env:TEMP "llvm-mingw.zip"
        $extractTo = Join-Path $env:TEMP "llvm-mingw-extract"
        Invoke-WebRequest -Uri $llvmUrl -OutFile $zipPath
        if (Test-Path $extractTo) { Remove-Item -Recurse -Force $extractTo }
        Expand-Archive -Path $zipPath -DestinationPath $extractTo -Force
        $inner = Get-ChildItem $extractTo | Select-Object -First 1
        if (-not $inner) { throw "llvm-mingw zip layout unexpected" }
        if (Test-Path $LlvmMingwRoot) { Remove-Item -Recurse -Force $LlvmMingwRoot }
        Move-Item $inner.FullName $LlvmMingwRoot
        # Machine PATH (Join-Path avoids "$var\bin" parse issues in some hosts/encodings)
        $llvmBin = Join-Path $LlvmMingwRoot "bin"
        $machinePath = [Environment]::GetEnvironmentVariable("Path", "Machine")
        if ($machinePath -notlike ("*{0}*" -f $llvmBin)) {
            $newPath = if ([string]::IsNullOrEmpty($machinePath)) { $llvmBin } else { $machinePath + ";" + $llvmBin }
            [Environment]::SetEnvironmentVariable("Path", $newPath, "Machine")
        }
        Refresh-Path
    }
    # Always publish root for CI discovery (existing or freshly installed)
    if (Test-Path $LlvmMingwRoot) {
        [Environment]::SetEnvironmentVariable("LLVM_MINGW_ROOT", $LlvmMingwRoot, "Machine")
        $env:LLVM_MINGW_ROOT = $LlvmMingwRoot
        Write-Step "Set machine env LLVM_MINGW_ROOT=$LlvmMingwRoot"
    }
} else {
    Write-Step "Skipping llvm-mingw (-SkipLlvmMingw)"
}

# ---------------------------------------------------------------------------
# Rust
# ---------------------------------------------------------------------------
function Install-Rust {
    Refresh-Path
    $cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
    $rustup = Join-Path $cargoBin "rustup.exe"
    if (-not (Test-Path $rustup)) {
        Write-Step "Installing rustup..."
        $rustupInit = Join-Path $env:TEMP "rustup-init.exe"
        Invoke-WebRequest -Uri "https://win.rustup.rs/x86_64" -OutFile $rustupInit
        & $rustupInit -y --default-toolchain stable --profile default
        if ($LASTEXITCODE -ne 0) { throw "rustup-init failed: $LASTEXITCODE" }
    } else {
        Write-Step "rustup already present"
    }
    Refresh-Path
    $env:Path = $cargoBin + ";" + $env:Path

    Write-Step "Updating stable + Windows MSVC targets (x64 + arm64)"
    & rustup toolchain install stable --profile default
    & rustup default stable
    & rustup update stable
    & rustup target add x86_64-pc-windows-msvc
    & rustup target add aarch64-pc-windows-msvc

    & rustc -vV
    & cargo -vV
    & rustup show
}

Install-Rust

if (-not $SkipTauriCli) {
    Write-Step "Installing tauri-cli ^2"
    Refresh-Path
    & cargo install tauri-cli --version "^2" --locked
    if ($LASTEXITCODE -ne 0) { throw "cargo install tauri-cli failed" }
    & cargo tauri --version
} else {
    Write-Step "Skipping tauri-cli (-SkipTauriCli)"
}

# ---------------------------------------------------------------------------
# Verify
# ---------------------------------------------------------------------------
Write-Step "Verification"
Refresh-Path
$failed = $false
function Check([string]$Name, [scriptblock]$Test) {
    try {
        if (& $Test) {
            Write-Host "  OK  $Name"
        } else {
            Write-Host "  FAIL $Name" -ForegroundColor Red
            $script:failed = $true
        }
    } catch {
        Write-Host "  FAIL $Name ($_)" -ForegroundColor Red
        $script:failed = $true
    }
}

Check "git" { Assert-Command "git" }
Check "cmake" { Assert-Command "cmake" }
Check "ninja" { Assert-Command "ninja" }
Check "msys2 bash" { Test-Path $Bash }
Check "mingw gcc" { Test-Path (Join-Path $MsysRoot "mingw64\bin\gcc.exe") }
Check "mingw g++" { Test-Path (Join-Path $MsysRoot "mingw64\bin\g++.exe") }
Check "libusb.h" { Test-Path (Join-Path $MsysRoot "mingw64\include\libusb-1.0\libusb.h") }
Check "libusb-1.0.dll" { Test-Path (Join-Path $MsysRoot "mingw64\bin\libusb-1.0.dll") }
Check "libusb-1.0.a (static)" { Test-Path (Join-Path $MsysRoot "mingw64\lib\libusb-1.0.a") }
Check "pkg-config" { Test-Path (Join-Path $MsysRoot "mingw64\bin\pkg-config.exe") }
Check "msys make" { Test-Path (Join-Path $MsysRoot "usr\bin\make.exe") }
Check "msys autoconf" { Test-Path (Join-Path $MsysRoot "usr\bin\autoconf") }
Check "msys automake" { Test-Path (Join-Path $MsysRoot "usr\bin\automake") }
Check "msys m4" { Test-Path (Join-Path $MsysRoot "usr\bin\m4.exe") }
Check "msys bzip2" { Test-Path (Join-Path $MsysRoot "usr\bin\bzip2.exe") }
Check "msys tar" { Test-Path (Join-Path $MsysRoot "usr\bin\tar.exe") }
Check "msys curl" { Test-Path (Join-Path $MsysRoot "usr\bin\curl.exe") }
Check "msys zip" { Test-Path (Join-Path $MsysRoot "usr\bin\zip.exe") }
Check "rustup" { Assert-Command "rustup" }
Check "rustc" { Assert-Command "rustc" }
Check "cargo" { Assert-Command "cargo" }
Check "target x86_64-msvc" { (& rustup target list --installed) -match "x86_64-pc-windows-msvc" }
Check "target aarch64-msvc" { (& rustup target list --installed) -match "aarch64-pc-windows-msvc" }
if (-not $SkipLlvmMingw) {
    Check "aarch64-w64-mingw32-gcc" {
        (Test-Path "C:\llvm-mingw\bin\aarch64-w64-mingw32-gcc.exe") -or (Assert-Command "aarch64-w64-mingw32-gcc")
    }
    Check "aarch64-w64-mingw32-g++" {
        (Test-Path "C:\llvm-mingw\bin\aarch64-w64-mingw32-g++.exe") -or (Assert-Command "aarch64-w64-mingw32-g++")
    }
}
if (-not $SkipTauriCli) {
    Check "tauri-cli" {
        $null = & cargo tauri --version 2>$null
        $LASTEXITCODE -eq 0
    }
}

if ($failed) {
    Write-Step "Some checks failed - review FAIL lines above."
    exit 1
}

Write-Step "All checks passed."
Write-Host ""
Write-Host "Satisfies runner expectations for:"
Write-Host "  build-rkdeveloptool.yaml (windows-x86_64, windows-aarch64)"
Write-Host "  portable.yml / installer.yml (Tauri + rkdeveloptool)"
Write-Host ""
Write-Host "Next (in the repo):"
Write-Host "  git submodule update --init --recursive"
Write-Host "  cargo tauri build --no-bundle --target x86_64-pc-windows-msvc"
Write-Host "  cargo tauri build --no-bundle --target aarch64-pc-windows-msvc"
Write-Host "  # rkdeveloptool: MSYS2 MinGW (x64) or llvm-mingw (arm64); see build-rkdeveloptool.yaml"
Write-Host ""
Write-Host "Open a NEW shell so PATH picks up Git, cargo, llvm-mingw, etc."
