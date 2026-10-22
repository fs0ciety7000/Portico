#!/usr/bin/env bash
# Render the AUR packages portico-bin (TUI) and portico-gui-bin (GUI) for a
# published Portico release: PKGBUILD from the templates next to this script,
# checksums from the release's SHA256SUMS, and .SRCINFO computed from the
# rendered PKGBUILD (same fields and order as `makepkg --printsrcinfo`).
# Needs bash, awk, sed, grep and sha256sum only (no makepkg).
#
#   packaging/aur/render.sh <version> <SHA256SUMS> [out-dir]
#
# <version>    X.Y.Z (no leading v, no pre-release: pacman forbids '-' in pkgver)
# <SHA256SUMS> the file attached to the GitHub Release
# [out-dir]    default: dist/aur (gets portico-bin/ and portico-gui-bin/)
#
# Environment:
#   PORTICO_REPO_URL  default https://github.com/fs0ciety7000/Portico
#   AUR_MAINTAINER    "Name <email>" for the Maintainer line (default: Portico contributors)
#   PKGREL            default 1 (bump by hand to republish the same version)
#   LICENSE_DIR       directory holding LICENSE-MIT and LICENSE-APACHE exactly as
#                     they are at the tag v<version> (default: the repository root)
set -euo pipefail

die() { printf 'render.sh: %s\n' "$*" >&2; exit 1; }

if [ $# -lt 2 ] || [ $# -gt 3 ]; then die "usage: $0 <version> <SHA256SUMS> [out-dir]"; fi
version=$1
sums=$2
out=${3:-dist/aur}
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_url=${PORTICO_REPO_URL:-https://github.com/fs0ciety7000/Portico}
maintainer=${AUR_MAINTAINER:-Portico contributors}
pkgrel=${PKGREL:-1}
license_dir=${LICENSE_DIR:-$here/../..}

[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "version must be X.Y.Z, got '$version'"
[[ $pkgrel =~ ^[1-9][0-9]*$ ]] || die "PKGREL must be a positive integer, got '$pkgrel'"
[[ $repo_url =~ ^https://github\.com/([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)$ ]] \
  || die "PORTICO_REPO_URL must be https://github.com/<owner>/<repo>, got '$repo_url'"
raw_url="https://raw.githubusercontent.com/${BASH_REMATCH[1]}"
# The maintainer ends up in a shell comment and a sed replacement.
[[ $maintainer =~ ^[A-Za-z0-9\ ._@\<\>+-]+$ ]] || die "AUR_MAINTAINER has unexpected characters"
[ -f "$sums" ] || die "no such file: $sums"

sha_of() {
  local sha
  sha=$(awk -v f="$1" '{ n = $2; sub(/^\*/, "", n) } n == f { print $1 }' "$sums")
  [ -n "$sha" ] || die "$1 is not listed in $sums"
  [ "$(printf '%s\n' "$sha" | wc -l)" -eq 1 ] || die "$1 is listed more than once in $sums"
  [[ $sha =~ ^[0-9a-f]{64}$ ]] || die "malformed SHA-256 for $1"
  printf '%s' "$sha"
}
file_sha() {
  [ -f "$1" ] || die "no such file: $1"
  sha256sum "$1" | awk '{ print $1 }'
}

sha_tui_x86_64=$(sha_of "portico-$version-x86_64-unknown-linux-gnu.tar.gz")
sha_tui_aarch64=$(sha_of "portico-$version-aarch64-unknown-linux-gnu.tar.gz")
sha_deb_amd64=$(sha_of "Portico_${version}_amd64.deb")
sha_deb_arm64=$(sha_of "Portico_${version}_arm64.deb")
sha_mit=$(file_sha "$license_dir/LICENSE-MIT")
sha_apache=$(file_sha "$license_dir/LICENSE-APACHE")

render_pkgbuild() {
  sed \
    -e "s|@VERSION@|$version|g" \
    -e "s|@PKGREL@|$pkgrel|g" \
    -e "s|@REPO_URL@|$repo_url|g" \
    -e "s|@RAW_URL@|$raw_url|g" \
    -e "s|@MAINTAINER@|$maintainer|g" \
    -e "s|@SHA_X86_64_UNKNOWN_LINUX_GNU@|$sha_tui_x86_64|g" \
    -e "s|@SHA_AARCH64_UNKNOWN_LINUX_GNU@|$sha_tui_aarch64|g" \
    -e "s|@SHA_DEB_AMD64@|$sha_deb_amd64|g" \
    -e "s|@SHA_DEB_ARM64@|$sha_deb_arm64|g" \
    -e "s|@SHA_LICENSE_MIT@|$sha_mit|g" \
    -e "s|@SHA_LICENSE_APACHE@|$sha_apache|g" \
    "$1" | sed -e '/^#%/d'
}

# srcinfo <PKGBUILD>: .SRCINFO of a single-package PKGBUILD without
# package-level overrides, in makepkg's order (scripts/libmakepkg/srcinfo.sh).
# Only sources a PKGBUILD this script just rendered from its own template.
# shellcheck disable=SC2154 # pkgname, arch: set by the sourced PKGBUILD
srcinfo() (
  set +u
  # shellcheck disable=SC1090 # the rendered PKGBUILD
  source "$1"
  attr() { # <name> <values...>: one line per value, whitespace normalised
    local name=$1 v
    shift
    for v in "$@"; do
      v=$(printf '%s' "$v" | tr -s '[:space:]' ' ' | sed -e 's/^ //' -e 's/ $//')
      printf '\t%s = %s\n' "$name" "$v"
    done
  }
  values() { local -n ref=$1; [ "${#ref[@]}" -eq 0 ] || attr "$1" "${ref[@]}"; }
  printf 'pkgbase = %s\n' "$pkgname"
  for name in pkgdesc pkgver pkgrel epoch url install changelog; do
    if [ -n "${!name}" ]; then attr "$name" "${!name}"; fi
  done
  for name in arch groups license checkdepends makedepends depends optdepends \
      provides conflicts replaces noextract options backup source validpgpkeys sha256sums; do
    declare -p "$name" > /dev/null 2>&1 && values "$name"
  done
  for a in "${arch[@]}"; do
    for name in source provides conflicts depends replaces optdepends makedepends checkdepends sha256sums; do
      declare -p "${name}_$a" > /dev/null 2>&1 && values "${name}_$a"
    done
  done
  printf '\npkgname = %s\n' "$pkgname"
)

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
for pkg in portico-bin portico-gui-bin; do
  mkdir -p "$tmp/$pkg"
  render_pkgbuild "$here/$pkg/PKGBUILD" > "$tmp/$pkg/PKGBUILD"
  if grep -n '@[A-Z0-9_]*@' "$tmp/$pkg/PKGBUILD"; then die "$pkg: unreplaced placeholder (see above)"; fi
  bash -n "$tmp/$pkg/PKGBUILD" || die "$pkg: PKGBUILD is not valid bash"
  srcinfo "$tmp/$pkg/PKGBUILD" > "$tmp/$pkg/.SRCINFO"
done

for pkg in portico-bin portico-gui-bin; do
  mkdir -p "$out/$pkg"
  cp "$tmp/$pkg/PKGBUILD" "$tmp/$pkg/.SRCINFO" "$out/$pkg/"
  printf '%s\n' "$out/$pkg/PKGBUILD" "$out/$pkg/.SRCINFO"
done
