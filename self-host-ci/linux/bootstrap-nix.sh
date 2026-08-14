#!/usr/bin/env bash
# One-time: install multi-user (daemon) Nix on a self-hosted Linux runner so
# .github/workflows/nix.yaml can build and push the flake package.
#
#   bash self-host-ci/linux/bootstrap-nix.sh
#
# Run as a sudo-capable user (the installer escalates itself). No nix.conf
# changes are needed: the workflow enables flakes via NIX_CONFIG and puts
# /nix/var/nix/profiles/default/bin on PATH itself; cachix is provided by
# the cachix-action step.

set -euo pipefail

if command -v nix >/dev/null 2>&1 || [ -x /nix/var/nix/profiles/default/bin/nix ]; then
  echo "Nix already installed:"
  "$(command -v nix || echo /nix/var/nix/profiles/default/bin/nix)" --version
  exit 0
fi

curl -fsSL https://nixos.org/nix/install | sh -s -- --daemon --yes

echo
echo "Nix installed. No runner-service restart needed — the workflow adds"
echo "/nix/var/nix/profiles/default/bin to PATH per job."
/nix/var/nix/profiles/default/bin/nix --version
