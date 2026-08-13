# CI helpers

| File | Role |
|------|------|
| `sanitize.sh` | PATH / workspace helpers for self-hosted bash steps (sourced by all three workflows) |

All packaging logic lives in the workflows themselves:

```
package.yaml
  ├─ build-rkdeveloptool.yaml  → rkdeveloptool-<os>-<arch>   (make, static libusb)
  ├─ build-app.yaml            → app-<os>-<arch>             (cargo tauri build; .app on macOS)
  └─ package matrix (6 cells)  → stage folder → portable-* + installer-* artifacts
```

The package job stages one install folder per cell
(app + `rkdeveloptool` + `loader_binaries/`), zips it as the portable, then
builds the OS installer from the same stage.

### Portable zip contents

- **macOS:** `Rockchip Universal Imager.app` + `rkdeveloptool` + `loader_binaries/`
- **Linux:** `Rockchip Universal Imager` + `rkdeveloptool` + `loader_binaries/`
- **Windows:** `Rockchip Universal Imager.exe` + `rkdeveloptool.exe` + `loader_binaries/`

No `portable` marker.

### Installers

| OS | Tool | Output |
|----|------|--------|
| Windows | NSIS (`makensis`, `self-host-ci/windows/installer.nsi`) | `*-setup.exe` |
| macOS | `hdiutil` | `*.dmg` ("Rockchip Universal Imager/" folder + Applications symlink) |
| Linux | `dpkg-deb` | `*.deb` → `/opt/rockchip-universal-imager` |

macOS signing/notarization is driven by repo secrets — see the header comment
in `package.yaml`.

### Linux runners

| Product | Runner labels |
|---------|----------------|
| `linux-x86_64` | `[self-hosted, Linux, X64]` |
| `linux-aarch64` | `[self-hosted, Linux, ARM64]` (native app + companion) |

Bootstrap both with `self-host-ci/linux/bootstrap-build-deps.sh`.
