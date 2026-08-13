# Shared path/env helpers for CI bash steps (Linux / macOS / MSYS2 Windows).
# Part of self-host-ci/ (same family as bootstrap-build-deps.* scripts).
#
#   source self-host-ci/sanitize.sh
#
# Goals:
#   - No hard dependency on Desktop / a fixed drive letter
#   - Discover MSYS2 / llvm-mingw / Homebrew via env + common locations
#   - Normalize Windows paths to Unix form for bash
#   - Pure bash only at bootstrap (GHA uses bash --noprofile --norc; tr/cygpath
#     may be missing until MSYS usr/bin is on PATH)

# ---------------------------------------------------------------------------
# Minimal PATH bootstrap (no external commands required)
# ---------------------------------------------------------------------------
ci_path_prepend() {
  local d="$1"
  [[ -n "$d" && -d "$d" ]] || return 0
  case ":${PATH:-}:" in
    *":$d:"*) ;;
    *) export PATH="$d${PATH:+:$PATH}" ;;
  esac
}

# Pure bash ASCII lowercase (avoids `tr`, missing under --noprofile --norc).
ci_tolower() {
  local s="$1" out="" i c
  local -i i
  for ((i = 0; i < ${#s}; i++)); do
    c="${s:i:1}"
    case "$c" in
      A) out+=a ;; B) out+=b ;; C) out+=c ;; D) out+=d ;; E) out+=e ;;
      F) out+=f ;; G) out+=g ;; H) out+=h ;; I) out+=i ;; J) out+=j ;;
      K) out+=k ;; L) out+=l ;; M) out+=m ;; N) out+=n ;; O) out+=o ;;
      P) out+=p ;; Q) out+=q ;; R) out+=r ;; S) out+=s ;; T) out+=t ;;
      U) out+=u ;; V) out+=v ;; W) out+=w ;; X) out+=x ;; Y) out+=y ;;
      Z) out+=z ;;
      *) out+="$c" ;;
    esac
  done
  printf '%s' "$out"
}

# Prefer bash ${var,,} when available (bash 4+).
if [[ ${BASH_VERSINFO[0]} -ge 4 ]]; then
  ci_tolower() { printf '%s' "${1,,}"; }
fi

# Put MSYS2 core tools on PATH early so later steps can use cygpath/make/etc.
ci_bootstrap_msys_path() {
  [[ "$(uname -s 2>/dev/null)" == MINGW* || "$(uname -s 2>/dev/null)" == MSYS* || -n "${WINDIR:-}${windir:-}" ]] || return 0

  local candidates=()
  # Env may still be Windows-style; convert without needing cygpath.
  if [[ -n "${MSYS2_ROOT:-}" ]]; then
    candidates+=("${MSYS2_ROOT}")
  fi
  candidates+=(
    /usr/bin
    /mingw64/bin
    /c/msys64/usr/bin
    /c/msys64/mingw64/bin
    /d/msys64/usr/bin
    /d/msys64/mingw64/bin
  )

  local c u
  for c in "${candidates[@]}"; do
    u="$(ci_to_unix_path "$c")"
    # usr/bin style
    if [[ -d "$u" ]]; then
      ci_path_prepend "$u"
    fi
    # If root given, also add usr/bin + mingw64/bin
    if [[ -d "$u/usr/bin" ]]; then
      ci_path_prepend "$u/usr/bin"
      ci_path_prepend "$u/mingw64/bin"
    fi
  done
}

# Convert Windows path to Unix (/c/Users/...) when needed. No external tools.
ci_to_unix_path() {
  local p="${1:-}"
  [[ -n "$p" ]] || { echo ""; return 0; }

  # Strip surrounding quotes
  p="${p%\"}"
  p="${p#\"}"
  p="${p%\'}"
  p="${p#\'}"

  # Already MSYS/Git style: /c/Users/... or //c/Users (normalize //c -> /c)
  if [[ "$p" =~ ^//[A-Za-z]/ ]]; then
    p="/${p:1}"
  fi
  if [[ "$p" =~ ^/[a-zA-Z]/ ]]; then
    printf '%s\n' "$p"
    return 0
  fi

  # Prefer cygpath when available (after PATH bootstrap)
  if command -v cygpath >/dev/null 2>&1; then
    local out
    out="$(cygpath -u "$p" 2>/dev/null || true)"
    if [[ -n "$out" ]]; then
      printf '%s\n' "$out"
      return 0
    fi
  fi

  # C:\foo or C:/foo → /c/foo
  if [[ "$p" =~ ^[A-Za-z]:[\\/] || "$p" =~ ^[A-Za-z]:$ ]]; then
    local drive rest
    drive="$(ci_tolower "${p:0:1}")"
    rest="${p:2}"
    rest="${rest//\\//}"
    # Ensure single slash between drive and rest
    rest="${rest#/}"
    printf '/%s/%s\n' "$drive" "$rest"
    return 0
  fi

  # Mixed junk like \Users\... (missing drive) — do not invent //Users
  if [[ "$p" == \\* || "$p" == /* ]]; then
    printf '%s\n' "${p//\\//}"
    return 0
  fi

  printf '%s\n' "${p//\\//}"
}

# Resolve repo workspace (Actions always sets GITHUB_WORKSPACE + job cwd).
ci_workspace() {
  # 1) Prefer pwd when we are already in the repo (most reliable under MSYS2).
  #    Actions sets the step working directory to GITHUB_WORKSPACE.
  if [[ -f self-host-ci/sanitize.sh || -f .github/workflows/build-rkdeveloptool.yaml || -d .git || -f .git ]]; then
    if command -v pwd >/dev/null 2>&1; then
      local here
      here="$(pwd -P 2>/dev/null || pwd)"
      if [[ -n "$here" && -d "$here" ]]; then
        # Reject broken //Users/... forms
        if [[ "$here" != //Users/* && "$here" != //users/* ]]; then
          printf '%s\n' "$here"
          return 0
        fi
      fi
    fi
  fi

  local raw="${GITHUB_WORKSPACE:-${GITHUB_WORKSPACE_RAW:-}}"
  if [[ -z "$raw" ]]; then
    raw="$(pwd -P 2>/dev/null || pwd)"
  fi
  local u
  u="$(ci_to_unix_path "$raw")"

  # If conversion produced a non-existent path, fall back to pwd
  if [[ ! -d "$u" ]]; then
    local here
    here="$(pwd -P 2>/dev/null || pwd)"
    if [[ -d "$here" ]]; then
      printf '%s\n' "$here"
      return 0
    fi
  fi
  printf '%s\n' "$u"
}

# Discover MSYS2 root (Windows only). Prints unix path or empty.
ci_find_msys2_root() {
  local candidates=()

  if [[ -n "${MSYS2_ROOT:-}" ]]; then
    candidates+=("$(ci_to_unix_path "$MSYS2_ROOT")")
  fi
  if [[ -n "${MSYS2_BASH:-}" ]]; then
    local b parent
    b="$(ci_to_unix_path "$MSYS2_BASH")"
    if [[ -f "$b" || -x "$b" ]]; then
      parent="$(cd "$(dirname "$b")/../.." 2>/dev/null && pwd || true)"
      [[ -n "$parent" ]] && candidates+=("$parent")
    fi
  fi

  local sd="${SYSTEMDRIVE:-${SystemDrive:-C:}}"
  candidates+=("$(ci_to_unix_path "${sd}/msys64")")
  candidates+=(/c/msys64 /d/msys64 /e/msys64 /msys64)

  local c
  for c in "${candidates[@]}"; do
    [[ -n "$c" ]] || continue
    if [[ -d "$c/mingw64/bin" || -f "$c/usr/bin/bash.exe" || -x "$c/usr/bin/bash" ]]; then
      printf '%s\n' "$c"
      return 0
    fi
  done
  return 1
}

# Discover llvm-mingw root (Windows aarch64 cross). Prints unix path or empty.
ci_find_llvm_mingw_root() {
  local candidates=()

  if [[ -n "${LLVM_MINGW_ROOT:-}" ]]; then
    candidates+=("$(ci_to_unix_path "$LLVM_MINGW_ROOT")")
  fi

  local sd="${SYSTEMDRIVE:-${SystemDrive:-C:}}"
  candidates+=("$(ci_to_unix_path "${sd}/llvm-mingw")")
  candidates+=(/c/llvm-mingw /d/llvm-mingw /e/llvm-mingw /llvm-mingw)
  if [[ -n "${PROGRAMFILES:-}" ]]; then
    candidates+=("$(ci_to_unix_path "${PROGRAMFILES}/llvm-mingw")")
  fi

  local c
  for c in "${candidates[@]}"; do
    [[ -n "$c" ]] || continue
    if [[ -f "$c/bin/aarch64-w64-mingw32-gcc.exe" || -x "$c/bin/aarch64-w64-mingw32-gcc" ]]; then
      printf '%s\n' "$c"
      return 0
    fi
  done
  return 1
}

# Set PATH / ACLOCAL for the current OS_LABEL (linux|macos|windows).
#
# Optional env:
#   CI_SKIP_LLVM_MINGW=1  — do not put llvm-mingw on PATH (required for Tauri /
#                           cargo-install host builds; llvm-mingw's clang wrappers
#                           break x86_64-w64-mingw32-gcc / -lgcc linking).
#                           Still used by build-rkdeveloptool for windows-aarch64.
ci_setup_toolchain_path() {
  local os="${1:-${OS_LABEL:-}}"
  case "$os" in
    windows)
      ci_bootstrap_msys_path
      local msys llvm cargo_u
      msys="$(ci_find_msys2_root || true)"
      if [[ -n "$msys" ]]; then
        export MSYS2_ROOT_UNIX="$msys"
        ci_path_prepend "$msys/usr/bin"
        ci_path_prepend "$msys/mingw64/bin"
        export ACLOCAL_PATH="${msys}/usr/share/aclocal:${msys}/mingw64/share/aclocal${ACLOCAL_PATH:+:$ACLOCAL_PATH}"
        export PKG_CONFIG_PATH="${msys}/mingw64/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
        echo "sanitize: MSYS2_ROOT_UNIX=$msys"
      else
        echo "sanitize: WARNING: MSYS2 root not found (set MSYS2_ROOT)" >&2
      fi

      if [[ "${CI_SKIP_LLVM_MINGW:-0}" == "1" ]]; then
        echo "sanitize: skipping llvm-mingw on PATH (CI_SKIP_LLVM_MINGW=1)"
      else
        llvm="$(ci_find_llvm_mingw_root || true)"
        if [[ -n "$llvm" ]]; then
          export LLVM_MINGW_ROOT_UNIX="$llvm"
          # Append (do not prepend): host gcc/msvc must win over llvm clang wrappers.
          # Prepend was breaking cargo install (host x86_64-gnu) with missing -lgcc.
          case ":${PATH:-}:" in
            *":$llvm/bin:"*) ;;
            *) export PATH="${PATH:+$PATH:}$llvm/bin" ;;
          esac
          echo "sanitize: LLVM_MINGW_ROOT_UNIX=$llvm (appended to PATH)"
        else
          echo "sanitize: WARNING: llvm-mingw not found (set LLVM_MINGW_ROOT) - windows-aarch64 rkdeveloptool may fail" >&2
        fi
      fi

      if [[ -n "${USERPROFILE:-}" ]]; then
        cargo_u="$(ci_to_unix_path "${USERPROFILE}/.cargo/bin")"
        ci_path_prepend "$cargo_u"
      elif [[ -n "${HOME:-}" ]]; then
        ci_path_prepend "${HOME}/.cargo/bin"
      fi
      ;;
    macos)
      if [[ -x /opt/homebrew/bin/brew ]]; then
        # shellcheck disable=SC1091
        eval "$(/opt/homebrew/bin/brew shellenv)"
      elif [[ -x /usr/local/bin/brew ]]; then
        # shellcheck disable=SC1091
        eval "$(/usr/local/bin/brew shellenv)"
      fi
      ci_path_prepend "${HOME:-}/.cargo/bin"
      ;;
    linux)
      ci_path_prepend "${HOME:-}/.cargo/bin"
      ;;
  esac
}

# Prefer MSVC host toolchain on Windows for Tauri / cargo install (not mingw-gnu).
ci_prefer_windows_msvc_host() {
  local u
  u="$(uname -s 2>/dev/null || true)"
  case "$u" in
    MINGW*|MSYS*|CYGWIN*) ;;
    *)
      # Also honor explicit OS_LABEL from CI
      if [[ "${OS_LABEL:-}" != "windows" ]]; then
        return 0
      fi
      ;;
  esac
  if ! command -v rustup >/dev/null 2>&1; then
    return 0
  fi
  local host="x86_64-pc-windows-msvc"
  rustup toolchain install "stable-${host}" --profile minimal
  rustup default "stable-${host}"
  rustup target add x86_64-pc-windows-msvc aarch64-pc-windows-msvc 2>/dev/null || true
  # Clear mingw-oriented CC that may be inherited from the runner env
  unset CC CXX CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER || true
  echo "sanitize: host toolchain $(rustup show active-toolchain 2>/dev/null || true)"
}

# Append a directory to GITHUB_PATH so later Actions steps inherit it.
# Writes Unix path (MSYS/bash) and Windows path when possible (runner service).
ci_github_path_add() {
  local dir="$1"
  [[ -n "$dir" ]] || return 0
  # Expand ~ if present
  [[ "$dir" == ~* ]] && dir="${dir/#\~/$HOME}"
  [[ -d "$dir" ]] || return 0
  if [[ -n "${GITHUB_PATH:-}" ]]; then
    echo "$dir" >>"$GITHUB_PATH"
    if command -v cygpath >/dev/null 2>&1; then
      local win
      win="$(cygpath -w "$dir" 2>/dev/null || true)"
      if [[ -n "$win" ]]; then
        echo "$win" >>"$GITHUB_PATH"
      fi
    fi
  fi
  ci_path_prepend "$dir"
}

# Cargo/rustup bin dirs for this runner (Unix paths).
ci_cargo_bin_dirs() {
  local dirs=()
  if [[ -n "${CARGO_HOME:-}" ]]; then
    dirs+=("$(ci_to_unix_path "${CARGO_HOME}/bin")")
  fi
  if [[ -n "${HOME:-}" ]]; then
    dirs+=("${HOME}/.cargo/bin")
  fi
  if [[ -n "${USERPROFILE:-}" ]]; then
    dirs+=("$(ci_to_unix_path "${USERPROFILE}/.cargo/bin")")
  fi
  # MSYS often uses /home/runner vs /c/Users/...
  if [[ -d /c/Users ]]; then
    local u
    for u in /c/Users/*/.cargo/bin; do
      [[ -d "$u" ]] && dirs+=("$u")
    done
  fi
  local d seen=""
  for d in "${dirs[@]}"; do
    [[ -n "$d" && -d "$d" ]] || continue
    case " $seen " in
      *" $d "*) continue ;;
    esac
    seen+=" $d"
    printf '%s\n' "$d"
  done
}

# Put cargo/bin on PATH + GITHUB_PATH; source rustup env if present.
ci_ensure_cargo_path() {
  local d envf
  while IFS= read -r d; do
    [[ -n "$d" ]] || continue
    ci_github_path_add "$d"
  done < <(ci_cargo_bin_dirs)

  for envf in "${CARGO_HOME:-}/env" "${HOME:-}/.cargo/env"; do
    [[ -n "$envf" && -f "$envf" ]] || continue
    # shellcheck disable=SC1090
    source "$envf" 2>/dev/null || true
  done
  if [[ -n "${USERPROFILE:-}" ]]; then
    envf="$(ci_to_unix_path "${USERPROFILE}/.cargo/env")"
    if [[ -f "$envf" ]]; then
      # shellcheck disable=SC1090
      source "$envf" 2>/dev/null || true
    fi
  fi
  hash -r 2>/dev/null || true
}

# Install rustup (if needed) + stable toolchain + optional target; export PATH for later steps.
# Usage: ci_ensure_rust [target-triple]
ci_ensure_rust() {
  local target="${1:-}"
  ci_ensure_cargo_path

  if ! command -v rustup >/dev/null 2>&1; then
    echo "sanitize: installing rustup…"
    local default_tc=stable
    # Windows self-hosted: install MSVC host (Tauri expects msvc, not gnu).
    if [[ -n "${WINDIR:-}${windir:-}${USERPROFILE:-}" ]] || [[ "$(uname -s 2>/dev/null || true)" == MINGW* || "$(uname -s 2>/dev/null || true)" == MSYS* ]]; then
      default_tc=stable-x86_64-pc-windows-msvc
    fi
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --default-toolchain "$default_tc" --profile minimal
    ci_ensure_cargo_path
  fi

  if ! command -v rustup >/dev/null 2>&1; then
    echo "ERROR: rustup not on PATH after install" >&2
    echo "PATH=$PATH" >&2
    ci_cargo_bin_dirs >&2 || true
    exit 1
  fi

  # On Windows always prefer MSVC host before installing targets / tools
  if [[ -n "${WINDIR:-}${windir:-}${USERPROFILE:-}" ]] || [[ "$(uname -s 2>/dev/null || true)" == MINGW* || "$(uname -s 2>/dev/null || true)" == MSYS* ]]; then
    ci_prefer_windows_msvc_host
  else
    rustup toolchain install stable --profile minimal
    rustup default stable
  fi

  if [[ -n "$target" ]]; then
    rustup target add "$target"
  fi

  # Persist for subsequent Actions steps (rust-cache, build, …)
  ci_ensure_cargo_path

  if ! command -v rustc >/dev/null 2>&1; then
    echo "ERROR: rustc not found after rustup setup" >&2
    rustup which rustc || true
    echo "PATH=$PATH" >&2
    exit 1
  fi

  # Prefer rustup-managed rustc when "rustc" is a broken shim
  local rustc_bin
  rustc_bin="$(rustup which rustc 2>/dev/null || command -v rustc)"
  echo "sanitize: rustc=$rustc_bin"
  "$rustc_bin" -vV
  cargo -vV
}

# Run path bootstrap on source so ci_workspace/tr fixes apply immediately.
ci_bootstrap_msys_path
