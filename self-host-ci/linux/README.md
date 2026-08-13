# Linux packaging

## Build-server bootstrap

On a Debian/Ubuntu self-hosted runner (SSH as a normal sudo user, **not root**):

```bash
cd /path/to/hardware-helper
bash self-host-ci/linux/bootstrap-build-deps.sh
# optional: --skip-tauri-cli
# optional (x86_64 host only): --skip-cross
```

Works on both **x86_64** and **aarch64**. On ARM64 it skips aarch64-cross packages
and installs the native toolchain only.

### Broken third-party PPAs (PhotonVision / Qualcomm images)

If `apt-get update` fails with **403 Forbidden** on a Launchpad PPA
(`ubuntu-qcom-iot/qcom-noble-ppa`, etc.), disable that source then re-run bootstrap:

```bash
ls /etc/apt/sources.list.d/
sudo find /etc/apt/sources.list.d -iname '*qcom*noble*' -exec mv {} {}.disabled \;
# also fix duplicate qcom-ppa .list vs .sources if apt warns about that
sudo apt-get update
bash self-host-ci/linux/bootstrap-build-deps.sh
```

The bootstrap script continues after a failed update when possible, but a clean
`apt-get update` is still recommended.

### Runner labels (GitHub Actions)

| Machine | Labels (typical auto labels) | Builds |
|---------|------------------------------|--------|
| Linux x86_64 | `self-hosted`, `Linux`, `X64` | `linux-x86_64` app + companion + `.deb` |
| Linux aarch64 | `self-hosted`, `Linux`, `ARM64` | `linux-aarch64` app + companion + `.deb` |

### What bootstrap installs

| Area | Packages / tools |
|------|------------------|
| Build core | `build-essential`, autoconf/automake/libtool, pkg-config, zip/unzip, tar, bzip2 |
| USB / crypto | `libusb-1.0-0-dev`, `libudev-dev`, `libssl-dev` |
| Tauri 2 GUI | `libwebkit2gtk-4.1-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`, `patchelf` |
| Packaging | `dpkg-dev` (for `.deb`) |
| Rust | rustup stable + **native** target (`x86_64-unknown-linux-gnu` or `aarch64-unknown-linux-gnu`) |
| CLI | `tauri-cli` ^2 (unless `--skip-tauri-cli`) |
| x86_64 only (optional) | `gcc-aarch64-linux-gnu` + multiarch (legacy cross; not required if you have an ARM64 runner) |

Shared CI helpers: `self-host-ci/sanitize.sh`.

## Packaging

- **Portable zip:** app + `rkdeveloptool` + `loader_binaries/`
- **Installer:** `.deb` → `/opt/rockchip-universal-imager/` + `.desktop` entry  
  (`Architecture: amd64` or `arm64`, set by the package job in `package.yaml`)
