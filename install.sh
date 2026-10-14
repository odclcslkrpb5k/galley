#!/usr/bin/env bash
# Install Galley into ~/.local (no root needed):
#
#   curl -fsSL https://raw.githubusercontent.com/odclcslkrpb5k/galley/main/install.sh | bash
#
# Environment (all optional):
#   GBK_VERSION=<hash>   install that release instead of the latest
#   GBK_PREFIX=<dir>     install under <dir> instead of ~/.local
#   GBK_FROM=<dir>       install files from <dir> instead of downloading (scripts/install)
#   GBK_ANY_OS=1         skip the Ubuntu 26.04 check
#
# Everything is inside main(), called on the last line: if the download of this script is
# cut short, bash has nothing complete to run.
set -euo pipefail

main() {
  local repo=odclcslkrpb5k/galley
  local prefix=${GBK_PREFIX:-$HOME/.local}
  local version=${GBK_VERSION:-}

  say() { printf '%s\n' "$*" >&2; }
  die() { say "galley install: $*"; exit 1; }

  # The prefix goes verbatim into the .desktop Exec line and a sed replacement: allow only
  # characters that need no quoting in either.
  [[ $prefix == /* && ! $prefix =~ [^A-Za-z0-9/._+-] ]] \
    || die "install location must be an absolute path of [A-Za-z0-9/._+-]: $prefix"
  [[ -z $version || $version =~ ^[0-9a-f]{12}$ ]] \
    || die "GBK_VERSION must be a 12-character build hash, got: $version"

  local src
  if [[ -n ${GBK_FROM:-} ]]; then
    src=$GBK_FROM
  else
    # Release binaries are built on Ubuntu 26.04 (x86_64) against its GTK and GLib.
    [[ $(uname -m) == x86_64 ]] || die "release binaries are x86_64 only (this is $(uname -m))"
    if [[ -z ${GBK_ANY_OS:-} ]]; then
      local id="" ver=""
      if [[ -r /etc/os-release ]]; then
        id=$(. /etc/os-release && printf '%s' "${ID:-}")
        ver=$(. /etc/os-release && printf '%s' "${VERSION_ID:-}")
      fi
      [[ $id == ubuntu && $ver == 26.04 ]] || die "release binaries are built for Ubuntu 26.04;
this is ${id:-unknown} ${ver:-}. They may not start here. To try anyway: GBK_ANY_OS=1.
Or build from source: see https://github.com/$repo#building"
    fi
    command -v curl >/dev/null || die "curl is required"
    command -v sha256sum >/dev/null || die "sha256sum is required"

    local base
    if [[ -n $version ]]; then
      base=https://github.com/$repo/releases/download/$version
    else
      base=https://github.com/$repo/releases/latest/download
    fi
    src=$(mktemp -d)
    # Expanded now: `src` is local and gone by the time the EXIT trap runs.
    trap "rm -rf '$src'" EXIT
    local f
    for f in galley galley.desktop.in LICENSE THIRD-PARTY-NOTICES.md SHA256SUMS; do
      curl -fsSL --proto '=https' --tlsv1.2 -o "$src/$f" "$base/$f" \
        || die "download failed: $base/$f"
    done
    # Catches corrupted or truncated downloads. (The sums come from the same release, so
    # this is not a defence against a compromised GitHub account.)
    (cd "$src" && sha256sum --quiet --strict -c SHA256SUMS) || die "checksum mismatch"
  fi

  [[ -f $src/galley && -f $src/galley.desktop.in ]] || die "missing files in $src"
  chmod 755 "$src/galley"
  local got
  got=$("$src/galley" --version) || die "the binary doesn't run here (missing libraries?)"

  local bindir=$prefix/bin appdir=$prefix/share/applications
  mkdir -p "$bindir" "$appdir"
  # Copy beside the target, then rename over it: atomic, and running instances keep the old
  # file instead of seeing a half-written one.
  cp "$src/galley" "$bindir/.galley.new"
  chmod 755 "$bindir/.galley.new"
  mv -f "$bindir/.galley.new" "$bindir/galley"
  sed "s|@BIN@|$bindir/galley|g" "$src/galley.desktop.in" > "$appdir/.galley.desktop.new"
  mv -f "$appdir/.galley.desktop.new" "$appdir/galley.desktop"
  if command -v update-desktop-database >/dev/null; then
    update-desktop-database "$appdir" 2>/dev/null || true
  fi

  say "installed $got"
  say "  $bindir/galley"
  say "  $appdir/galley.desktop"
  say "To make it the default for Markdown files:"
  say "  xdg-mime default galley.desktop text/markdown text/x-markdown"
}

main "$@"
