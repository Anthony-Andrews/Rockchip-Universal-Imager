# Windows packaging

## Build-server bootstrap

Elevated PowerShell:

```powershell
Set-ExecutionPolicy -Scope Process Bypass -Force
.\self-host-ci\windows\bootstrap-build-deps.ps1
```

Installs VS Build Tools, MSYS2 MinGW + libusb, llvm-mingw, rustup, tauri-cli,
and **NSIS** (`makensis`) for `package.yaml` installers.

## Packaging

- `installer.nsi` — NSIS script used by the package job in `.github/workflows/package.yaml`
- Portable: zip of `rockchip-universal-imager.exe` + `rkdeveloptool.exe` + `loader_binaries/`
- Installer: `*-setup.exe` → `%ProgramFiles%\Rockchip Universal Imager\`
