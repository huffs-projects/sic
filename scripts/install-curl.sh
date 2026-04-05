#!/usr/bin/env bash
# Install sic from GitHub releases without eget: downloads the latest matching
# release asset, extracts the sic binary, and installs it to SIC_INSTALL_DIR.
#
# Release assets should follow eget-friendly names (OS_Arch in the filename), e.g.:
#   sic-0.1.0-linux_amd64.tar.gz
#   sic-darwin_arm64.tar.gz
#
# Requires: bash, curl, tar or unzip, and python3 (stdlib only, for GitHub JSON).
#
# Environment:
#   SIC_REPO          GitHub repo as owner/name (default: huffs-projects/sic)
#   SIC_INSTALL_DIR   Directory for the sic binary (default: ~/.local/bin)
#   GITHUB_TOKEN / GH_TOKEN  Optional; raises API rate limits
#
# Example:
#   curl -fsSL https://raw.githubusercontent.com/huffs-projects/sic/main/scripts/install-curl.sh | sh
#   SIC_INSTALL_DIR=/usr/local/bin sh ./scripts/install-curl.sh

set -euo pipefail

: "${SIC_REPO:=huffs-projects/sic}"
: "${SIC_INSTALL_DIR:=${HOME}/.local/bin}"

die() {
  echo "error: $*" >&2
  exit 1
}

command -v curl >/dev/null 2>&1 || die "curl is required"
command -v python3 >/dev/null 2>&1 || die "python3 is required (or use scripts/install-eget.sh)"

owner="${SIC_REPO%%/*}"
repo="${SIC_REPO#*/}"
[[ -n "$owner" && -n "$repo" && "$SIC_REPO" == *"/"* ]] || die "invalid SIC_REPO: use owner/repo (got: ${SIC_REPO})"

api_url="https://api.github.com/repos/${owner}/${repo}/releases/latest"

# Resolve OS/arch tokens used in common release filenames (see eget FAQ).
case "$(uname -s)" in
  Darwin) sic_os=darwin ;;
  Linux) sic_os=linux ;;
  FreeBSD) sic_os=freebsd ;;
  OpenBSD) sic_os=openbsd ;;
  NetBSD) sic_os=netbsd ;;
  *) die "unsupported OS: $(uname -s)" ;;
esac

case "$(uname -m)" in
  x86_64 | amd64) sic_arch=amd64 ;;
  arm64 | aarch64) sic_arch=arm64 ;;
  i386 | i686) sic_arch=386 ;;
  riscv64) sic_arch=riscv64 ;;
  *) die "unsupported CPU: $(uname -m)" ;;
esac

export SIC_OS="$sic_os"
export SIC_ARCH="$sic_arch"

tmp_dir="$(mktemp -d)"
cleanup() { rm -rf "$tmp_dir"; }
trap cleanup EXIT

json_path="${tmp_dir}/release.json"

curl_args=(
  -fsSL
  -H "Accept: application/vnd.github+json"
  -H "X-GitHub-Api-Version: 2022-11-28"
)
if [[ -n "${GITHUB_TOKEN:-}" ]]; then
  curl_args+=(-H "Authorization: Bearer ${GITHUB_TOKEN}")
elif [[ -n "${GH_TOKEN:-}" ]]; then
  curl_args+=(-H "Authorization: Bearer ${GH_TOKEN}")
fi

curl "${curl_args[@]}" "$api_url" -o "$json_path"

export JSON_PATH="$json_path"
result="$(python3 - <<'PY'
import json
import os
import sys


def score(name: str, os_name: str, arch: str) -> int:
    n = name.lower()
    s = -1
    if f"{os_name}_{arch}" in n or f"{os_name}-{arch}" in n:
        s = 100
    elif os_name == "darwin" and arch in n and ("darwin" in n or "macos" in n):
        s = 95
    else:
        # Rust / LLVM style triples (common on GitHub releases)
        triples = []
        if os_name == "darwin" and arch == "arm64":
            triples = ["aarch64-apple-darwin", "aarch64_apple_darwin"]
        elif os_name == "darwin" and arch == "amd64":
            triples = ["x86_64-apple-darwin", "x86_64_apple_darwin"]
        elif os_name == "linux" and arch == "amd64":
            triples = ["x86_64-unknown-linux-gnu", "x86_64-unknown-linux-musl", "x86_64_linux"]
        elif os_name == "linux" and arch == "arm64":
            triples = ["aarch64-unknown-linux-gnu", "aarch64-unknown-linux-musl", "aarch64_linux"]
        for t in triples:
            if t in n:
                s = 90
                break
    if s < 0:
        return -1
    if os_name == "linux" and "musl" in n:
        s -= 5
    if "sha256" in n or n.endswith(".txt") or "sbom" in n:
        return -1
    return s


def checksum_url_for(assets: list, asset_name: str) -> str:
    for suffix in (".sha256", ".sha256sum"):
        want = asset_name + suffix
        for a in assets:
            if a.get("name") == want:
                return a.get("browser_download_url") or ""
    return ""


with open(os.environ["JSON_PATH"], encoding="utf-8") as f:
    data = json.load(f)

os_name = os.environ["SIC_OS"]
arch = os.environ["SIC_ARCH"]
best = None
best_score = -1
for a in data.get("assets", []):
    name = a.get("name") or ""
    url = a.get("browser_download_url") or ""
    if not url:
        continue
    sc = score(name, os_name, arch)
    if sc > best_score:
        best_score = sc
        best = (name, url)

if not best or best_score < 0:
    sys.stderr.write(
        f"No release asset matched {os_name}_{arch} for this platform.\n"
        "Publish builds whose filenames include the OS and arch, e.g. "
        f"'{os_name}_{arch}', per https://github.com/zyedidia/eget#faq\n"
    )
    sys.exit(1)

asset_name, asset_url = best
sha_url = checksum_url_for(data.get("assets", []), asset_name)
print(asset_name)
print(asset_url)
print(sha_url)
PY
)"

asset_name="$(echo "$result" | sed -n '1p')"
asset_url="$(echo "$result" | sed -n '2p')"
sha_url="$(echo "$result" | sed -n '3p')"
[[ -n "$asset_url" ]] || die "failed to resolve download URL"

echo "Downloading ${asset_name} ..." >&2
archive="${tmp_dir}/${asset_name}"
curl "${curl_args[@]}" -L "$asset_url" -o "$archive"

if [[ -n "$sha_url" ]]; then
  echo "Verifying SHA-256 ..." >&2
  shas="${tmp_dir}/checksum.sha256"
  curl "${curl_args[@]}" "$sha_url" -o "$shas"
  (
    cd "$tmp_dir"
    if command -v sha256sum >/dev/null 2>&1; then
      sha256sum -c checksum.sha256
    else
      shasum -a 256 -c checksum.sha256
    fi
  ) || die "checksum verification failed"
fi

mkdir -p "${SIC_INSTALL_DIR}"
extract_dir="${tmp_dir}/out"
mkdir -p "$extract_dir"

case "$asset_name" in
  *.tar.gz | *.tgz)
    tar -xzf "$archive" -C "$extract_dir"
    ;;
  *.tar.xz)
    tar -xJf "$archive" -C "$extract_dir"
    ;;
  *.zip)
    unzip -q "$archive" -d "$extract_dir"
    ;;
  *)
    die "unsupported archive type: ${asset_name} (use .tar.gz, .tar.xz, or .zip)"
    ;;
esac

# Find sic binary (single top-level or nested)
sic_bin=""
while IFS= read -r -d '' f; do
  if [[ "$(basename "$f")" == "sic" ]]; then
    sic_bin="$f"
    break
  fi
done < <(find "$extract_dir" -type f -name sic -print0 2>/dev/null)

[[ -n "$sic_bin" ]] || die "could not find 'sic' binary inside ${asset_name}"

install -m 0755 "$sic_bin" "${SIC_INSTALL_DIR}/sic"
echo "Installed sic -> ${SIC_INSTALL_DIR}/sic" >&2
