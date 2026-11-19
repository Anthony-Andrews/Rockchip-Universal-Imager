# CI helpers

All workflows run on GitHub-hosted runners (no self-hosted machines).

| File | Role |
|------|------|
| `sanitize.sh` | PATH / workspace helpers for bash steps (Linux / macOS / MSYS2 Windows); sourced by `build-rkdeveloptool.yaml` |
| `*/bootstrap-build-deps.*` | Reference lists of build dependencies per OS. Useful for setting up a local dev machine; no longer used by CI |
| `linux/bootstrap-nix.sh` | Installs Nix on a Debian/Ubuntu box (local use) |

## Pipeline

```
release.yaml   (push tag v* → draft GitHub Release; workflow_dispatch → artifacts)
  ├─ build-rkdeveloptool.yaml  → rkdeveloptool-<os>-<arch>  (autotools, static libusb)
  └─ app matrix (6 cells)      → tauri-action: cargo tauri build
       macOS   (macos-15, arm64 native + x86_64 cross) → .app + .dmg
       Linux   (ubuntu-24.04, ubuntu-24.04-arm)         → .deb + .rpm + .AppImage
       Windows (windows-2025, windows-11-arm)           → NSIS setup.exe

nix.yaml       (workflow_dispatch) → nix build + push to Cachix (x86_64 + aarch64)
```

`rkdeveloptool` ships inside every bundle as a Tauri sidecar
(`bundle.externalBin`, see `src-tauri/binaries/README.md`) and
`dependencies/loader_binaries/` as a bundle resource (`bundle.resources`).
The `.deb` / `.rpm` also install the Rockchip udev rule from
`src-tauri/linux/`. Nothing is staged by hand.

macOS signing and notarization are optional and driven by repo secrets — see
the header comment in `release.yaml`.
