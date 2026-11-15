#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Install gitcko into $GITCKO_INSTALL_DIR (default ~/.local/bin).
# Always checks SHA-256. Checks the minisign signature when minisign or rsign
# is on PATH; otherwise prints how to check it. Never calls sudo.
# The pinned public key is a TEST key until Anthony replaces it before the
# first real tag (integrations/install/minisign.pub must match these halves).

set -eu

pinned_a='RWR5zJ23g86QtqzrU0bXyrUdiF'
pinned_b='cdCin2EF6VfTUVfHL6b9XDQxmBjKKK'
pinned="${pinned_a}${pinned_b}"

die() {
  printf '%s\n' "$1" >&2
  exit 1
}

script_dir() {
  case "$0" in
    sh|dash|bash|-sh|-dash|-bash|*/sh|*/dash|*/bash) return 1 ;;
  esac
  [ -f "$0" ] || return 1
  case "$0" in
    */*) CDPATH= cd -- "${0%/*}" && pwd ;;
    *) pwd ;;
  esac
}

if dir=$(script_dir); then
  if [ -f "$dir/minisign.pub" ]; then
    from_file=$(awk 'NF && $1 != "untrusted" { print; exit }' "$dir/minisign.pub")
    if [ "$from_file" != "$pinned" ]; then
      die "pinned public key does not match minisign.pub"
    fi
  fi
fi

[ -n "${HOME:-}" ] || die "install failed"

base=${GITCKO_DOWNLOAD_URL:-https://gitcko.dev/releases}
install_dir=${GITCKO_INSTALL_DIR:-$HOME/.local/bin}
while [ "$base" != "${base%/}" ]; do base=${base%/}; done

classify() {
  case "$base" in
    ''|*[![:print:]]*) die "download refused" ;;
  esac
  case "$base" in
    *://*@*) die "download refused" ;;
  esac
  case "$base" in
    /*)
      mode=dir
      root=$base
      ;;
    file://localhost/*)
      root="/${base#file://localhost/}"
      mode=dir
      ;;
    file:///*)
      root="/${base#file:///}"
      mode=dir
      ;;
    file://*)
      die "download refused"
      ;;
    https://*|http://*)
      rest=${base#*://}
      authority=${rest%%/*}
      [ -n "$authority" ] || die "download refused"
      case "$authority" in
        *[[:space:]]*) die "download refused" ;;
      esac
      port=
      case "$authority" in
        \[::1\]) host='[::1]' ;;
        \[::1\]:*)
          host='[::1]'
          port=${authority#\[::1\]:}
          ;;
        *:*)
          host=${authority%%:*}
          port=${authority#*:}
          ;;
        *) host=$authority ;;
      esac
      if [ -n "$port" ]; then
        case "$port" in
          *[!0-9]*) die "download refused" ;;
        esac
      fi
      case "$base" in
        https://*) mode=https ;;
        http://*)
          case "$host" in
            127.0.0.1|localhost|"[::1]") mode=loopback ;;
            *) die "download refused" ;;
          esac
          ;;
      esac
      root=$base
      ;;
    *) die "download refused" ;;
  esac
  if [ "$mode" = dir ]; then
    root=$(printf '%s' "$root" | awk '{ gsub(/%20/, " "); if (index($0, "%") > 0) exit 1; print }') || die "download refused"
  fi
}

classify

host_target() {
  os=$(uname -s)
  arch=$(uname -m)
  case "$os:$arch" in
    Darwin:arm64) printf '%s' aarch64-apple-darwin ;;
    Darwin:x86_64) printf '%s' x86_64-apple-darwin ;;
    Linux:x86_64) printf '%s' x86_64-unknown-linux-musl ;;
    Linux:aarch64|Linux:arm64) printf '%s' aarch64-unknown-linux-musl ;;
    *) return 1 ;;
  esac
}

target=$(host_target) || die "this build does not ship a binary for this system"
archive_name="gitcko-${target}.tar.gz"
bin_name="gitcko-${target}.bin"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

fetch() {
  name=$1
  dest=$2
  case "$name" in
    .|..|*[!A-Za-z0-9._-]*) die "download refused" ;;
  esac
  case "$mode" in
    dir)
      if [ -L "$root/$name" ]; then
        die "download refused"
      fi
      cp -- "$root/$name" "$dest"
      ;;
    https) curl -fsSL --proto '=https' --tlsv1.2 --max-redirs 0 -o "$dest" "$root/$name" >/dev/null 2>&1 || die "download failed" ;;
    loopback) curl -fsSL --proto '=http' --max-redirs 0 -o "$dest" "$root/$name" >/dev/null 2>&1 || die "download failed" ;;
    *) die "download refused" ;;
  esac
}

fetch SHA256SUMS "$work/SHA256SUMS"
fetch SHA256SUMS.minisig "$work/SHA256SUMS.minisig"
if [ ! -s "$work/SHA256SUMS.minisig" ]; then
  die "signature missing"
fi

tool=
if command -v minisign >/dev/null 2>&1; then
  tool=minisign
elif command -v rsign >/dev/null 2>&1; then
  tool=rsign
fi
if [ -n "$tool" ]; then
  if ! "$tool" -Vm "$work/SHA256SUMS" -x "$work/SHA256SUMS.minisig" -P "$pinned"; then
    die "signature rejected"
  fi
else
  printf '%s\n' "signature: not checked (minisign is not installed)." >&2
  printf '%s\n' "Check it with: minisign -Vm SHA256SUMS -P ${pinned} -x SHA256SUMS.minisig" >&2
  printf '%s\n' "Then run: gitcko doctor" >&2
fi

expect_hash() {
  awk -v name="$1" '
    { gsub(/\r/, "") }
    NF == 2 && length($1) == 64 && $1 ~ /^[0-9A-Fa-f]+$/ && $2 ~ /^[*]?[A-Za-z0-9._-]+$/ {
      fname = $2
      sub(/^\*/, "", fname)
      if (fname == name) { print tolower($1); found = 1; exit }
    }
    END { if (!found) exit 1 }
  ' "$work/SHA256SUMS"
}

hash_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -- "$1" | awk '{ print tolower($1) }'
  else
    shasum -a 256 -- "$1" | awk '{ print tolower($1) }'
  fi
}

fetch "$archive_name" "$work/$archive_name"
want=$(expect_hash "$archive_name") || die "checksum mismatch"
got=$(hash_file "$work/$archive_name")
if [ "$got" != "$want" ]; then
  die "checksum mismatch"
fi

names=$(tar -tzf "$work/$archive_name")
case "$names" in
  gitcko|./gitcko) ;;
  *) die "archive rejected" ;;
esac
extract=$work/extract
mkdir -p "$extract"
tar -xzf "$work/$archive_name" -C "$extract"
if [ -L "$extract/gitcko" ] || [ ! -f "$extract/gitcko" ]; then
  die "archive rejected"
fi
others=$(find "$extract" -mindepth 1 ! -path "$extract/gitcko" | wc -l | awk '{ print $1 }')
if [ "$others" != 0 ]; then
  die "archive rejected"
fi

want_bin=$(expect_hash "$bin_name") || die "checksum mismatch"
got_bin=$(hash_file "$extract/gitcko")
if [ "$got_bin" != "$want_bin" ]; then
  die "checksum mismatch"
fi

if [ -n "${XDG_DATA_HOME:-}" ]; then
  data=$XDG_DATA_HOME
else
  data=$HOME/.local/share
fi
release=$data/gitcko/release
printf '%s' "$install_dir$data" | awk 'index($0, "\n") > 0 { exit 1 }' || die "install failed"

mkdir -p "$install_dir" "$release"
cp -- "$work/SHA256SUMS" "$release/SHA256SUMS"
cp -- "$work/SHA256SUMS.minisig" "$release/SHA256SUMS.minisig"

dest=$install_dir/gitcko
if [ -L "$dest" ]; then
  die "refusing to replace a symlink"
fi
tmp=$install_dir/gitcko.new.$$
cp -- "$extract/gitcko" "$tmp"
chmod 755 "$tmp"
if ! mv -- "$tmp" "$dest"; then
  rm -f "$tmp"
  die "install failed"
fi

receipt=$install_dir/gitcko-install.txt
tmp_receipt=$install_dir/gitcko-install.txt.new.$$
printf '%s\n' 'method=script' "release_dir=${release}" > "$tmp_receipt"
mv -- "$tmp_receipt" "$receipt"
printf '%s\n' "installed: ${dest}"
