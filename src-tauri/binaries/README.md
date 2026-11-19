# Sidecar binaries

Tauri embeds `rkdeveloptool` as a sidecar (`bundle.externalBin` in
`tauri.conf.json`). `tauri-build` refuses to compile unless the binary for the
current target exists here, named with the target triple:

```
src-tauri/binaries/rkdeveloptool-<target-triple>[.exe]
```

| Target | File |
|--------|------|
| macOS arm64 | `rkdeveloptool-aarch64-apple-darwin` |
| macOS x86_64 | `rkdeveloptool-x86_64-apple-darwin` |
| Linux x86_64 | `rkdeveloptool-x86_64-unknown-linux-gnu` |
| Linux arm64 | `rkdeveloptool-aarch64-unknown-linux-gnu` |
| Windows x86_64 | `rkdeveloptool-x86_64-pc-windows-msvc.exe` |
| Windows arm64 | `rkdeveloptool-aarch64-pc-windows-msvc.exe` |

Your host triple: `rustc --print host-tuple`.

CI (`.github/workflows/release.yaml`) downloads the artifact produced by
`build-rkdeveloptool.yaml` into this directory. For local development either:

- download the matching `rkdeveloptool-<os>-<arch>` artifact from a CI run and
  copy the binary here under the name above, or
- build it from the submodule (`dependencies/rkdeveloptool`: `./autogen.sh &&
  ./configure && make`) and copy/symlink the result here.

Everything in this directory except this README is git-ignored.
