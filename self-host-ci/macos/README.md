# macOS packaging

## Build-server bootstrap

On a macOS self-hosted runner (SSH):

```bash
bash self-host-ci/macos/bootstrap-build-deps.sh
# optional: --skip-tauri-cli
```

Installs Xcode CLT, Homebrew packages (libusb, autotools), rustup + both
Apple targets, and `tauri-cli`.

Shared CI path helpers (used by GitHub Actions bash steps on all OSes):
`self-host-ci/sanitize.sh`.

## Packaging

- **App build:** `cargo tauri build --bundles app` → `Rockchip Universal Imager.app`
  (libusb is statically linked via rusb's `vendored` feature — the app has no
  external dylib dependencies)
- **Portable zip / install folder** (companions **beside** the `.app`, not inside it):
  - `Rockchip Universal Imager.app`
  - `rkdeveloptool`
  - `loader_binaries/`
- **Installer DMG:** one `Rockchip Universal Imager/` folder containing the
  three items above, plus an Applications symlink (KiCad-style: drag the
  whole folder into Applications)

The GUI looks for `rkdeveloptool` and `loader_binaries/` in the directory that
contains the `.app`. Users must keep that layout after install.

All packaging steps live in `.github/workflows/package.yaml` (stage folder →
portable zip → DMG). To reproduce locally, run the same commands from the
workflow's "Stage install folder" / "Build installer (macOS DMG)" steps.

## Signing / notarization

The package workflow signs in the stage, so the portable zip and the
DMG carry identical signed binaries:

- No env set → ad-hoc signatures (dev / unsigned distribution).
- `MACOS_SIGN_IDENTITY="Developer ID Application: … (TEAMID)"` → real
  signatures with hardened runtime + timestamp on the `.app`, `rkdeveloptool`,
  and the DMG itself.
- Additionally `MACOS_NOTARY_KEYCHAIN_PROFILE` (from
  `xcrun notarytool store-credentials`) **or**
  `APPLE_ID`/`APPLE_PASSWORD`/`APPLE_TEAM_ID` → the DMG is notarized
  (`notarytool submit --wait`) and stapled. Notarization registers the binary
  hashes with Apple, so the same binaries inside the portable zip pass
  Gatekeeper as well.

Set these as repo secrets (wired through in `package.yaml`).

Gatekeeper / first-open steps for unsigned builds: see root `README.md`.

Logs: `~/Library/Logs/RockchipUniversalImager`
