#!/usr/bin/env bash
# Install sic from GitHub releases using eget (https://github.com/zyedidia/eget).
#
# Prerequisites: eget on PATH (brew install eget, or see eget README).
#
# Environment:
#   SIC_REPO          GitHub repo as owner/name (default: huffs-projects/sic)
#   SIC_INSTALL_DIR   Directory for the sic binary (default: ~/.local/bin)
#   SIC_EGET_EXTRA    Extra args passed to eget (space-separated, quoted carefully)
#
# Example:
#   ./scripts/install-eget.sh
#   SIC_INSTALL_DIR=$HOME/bin ./scripts/install-eget.sh

set -euo pipefail

: "${SIC_REPO:=huffs-projects/sic}"
: "${SIC_INSTALL_DIR:=${HOME}/.local/bin}"

if ! command -v eget >/dev/null 2>&1; then
  echo "eget not found. Install it first, for example:" >&2
  echo "  brew install eget" >&2
  echo "  go install github.com/zyedidia/eget@latest" >&2
  echo "  https://github.com/zyedidia/eget#how-to-get-eget" >&2
  exit 1
fi

mkdir -p "${SIC_INSTALL_DIR}"

# Optional: SIC_EGET_EXTRA='--tag v0.1.0' or '--asset musl'
read -r -a extra_args <<< "${SIC_EGET_EXTRA:-}"

exec eget "${SIC_REPO}" \
  "${extra_args[@]}" \
  --file sic \
  --to "${SIC_INSTALL_DIR}"
