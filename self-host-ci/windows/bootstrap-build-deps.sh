#!/usr/bin/env bash
# Thin wrapper: run the PowerShell Windows bootstrap from Git Bash / MSYS2.
#
# Prefer elevated PowerShell for a full install:
#   powershell -ExecutionPolicy Bypass -File self-host-ci/windows/bootstrap-build-deps.ps1
#
# This script just re-invokes that .ps1 with the same flags.
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
PS1="$ROOT/bootstrap-build-deps.ps1"

if [[ ! -f "$PS1" ]]; then
  echo "Missing $PS1" >&2
  echo "Run this from the repo copy, e.g.:" >&2
  echo "  bash self-host-ci/windows/bootstrap-build-deps.sh" >&2
  echo "Do not nano a partial copy into \$HOME." >&2
  exit 1
fi

# PowerShell -File needs a Windows path (C:\...), not /c/Users/...
if command -v cygpath >/dev/null 2>&1; then
  PS1_WIN="$(cygpath -w "$PS1")"
elif [[ "$PS1" =~ ^/([a-zA-Z])/(.*)$ ]]; then
  # MSYS/Git Bash style /c/Users/... → C:\Users\...
  drive="${BASH_REMATCH[1]}"
  rest="${BASH_REMATCH[2]//\//\\}"
  PS1_WIN="${drive^^}:\\${rest}"
else
  PS1_WIN="$PS1"
fi

# Map bash-style flags to PowerShell switches
PS_ARGS=()
for arg in "$@"; do
  case "$arg" in
    --skip-tauri-cli)   PS_ARGS+=(-SkipTauriCli) ;;
    --skip-llvm-mingw)  PS_ARGS+=(-SkipLlvmMingw) ;;
    --skip-vs)          PS_ARGS+=(-SkipVsBuildTools) ;;
    --skip-winget)      PS_ARGS+=(-SkipWingetTools) ;;
    -h|--help)
      sed -n '2,12p' "$0"
      echo ""
      echo "PowerShell help: see comments at top of bootstrap-build-deps.ps1"
      exit 0
      ;;
    *)
      echo "Unknown option: $arg" >&2
      exit 2
      ;;
  esac
done

echo "Running elevated-capable bootstrap via PowerShell:"
echo "  $PS1_WIN"
echo "NOTE: open an *Administrator* PowerShell if winget/VS/PATH changes fail."

if command -v powershell.exe >/dev/null 2>&1; then
  exec powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$PS1_WIN" "${PS_ARGS[@]+"${PS_ARGS[@]}"}"
elif command -v pwsh.exe >/dev/null 2>&1; then
  exec pwsh.exe -NoProfile -ExecutionPolicy Bypass -File "$PS1_WIN" "${PS_ARGS[@]+"${PS_ARGS[@]}"}"
else
  echo "powershell.exe not found. Open an elevated PowerShell and run:" >&2
  echo "  $PS1_WIN" >&2
  exit 1
fi
