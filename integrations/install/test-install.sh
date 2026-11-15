#!/bin/sh
# Exercises install.sh against a local directory: a path with spaces, a bad
# checksum, a missing signature, and (when minisign is available) a real signature.
set -eu

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
INSTALL=$HERE/install.sh

fail() {
  printf 'FAIL %s\n' "$1" >&2
  exit 1
}

host_target() {
  os=$(uname -s)
  arch=$(uname -m)
  case "$os:$arch" in
    Darwin:arm64) printf '%s' aarch64-apple-darwin ;;
    Darwin:x86_64) printf '%s' x86_64-apple-darwin ;;
    Linux:x86_64) printf '%s' x86_64-unknown-linux-musl ;;
    Linux:aarch64|Linux:arm64) printf '%s' aarch64-unknown-linux-musl ;;
    *) fail "unsupported host" ;;
  esac
}

hash_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -- "$1" | awk '{ print tolower($1) }'
  else
    shasum -a 256 -- "$1" | awk '{ print tolower($1) }'
  fi
}

TARGET=$(host_target)
ARCHIVE="gitcko-${TARGET}.tar.gz"
BIN_SUM="gitcko-${TARGET}.bin"

build_release() {
  rel=$1
  payload=$2
  mkdir -p "$rel/stage"
  cp -- "$payload" "$rel/stage/gitcko"
  chmod 755 "$rel/stage/gitcko"
  tar -C "$rel/stage" -czf "$rel/$ARCHIVE" gitcko
  bin_hash=$(hash_file "$rel/stage/gitcko")
  arc_hash=$(hash_file "$rel/$ARCHIVE")
  printf '%s  %s\n' "$arc_hash" "$ARCHIVE" > "$rel/SHA256SUMS"
  printf '%s  %s\n' "$bin_hash" "$BIN_SUM" >> "$rel/SHA256SUMS"
  sort -o "$rel/SHA256SUMS" "$rel/SHA256SUMS"
}

assert_absent() {
  if [ -e "$1" ]; then
    fail "installed despite $2: $1"
  fi
}

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT INT TERM
PAYLOAD=$TMP/payload
printf '%s\n' '#!/bin/sh' 'echo gitcko-test-ok' > "$PAYLOAD"
if [ -n "${GITCKO_TEST_BIN:-}" ]; then
  cp -- "$GITCKO_TEST_BIN" "$PAYLOAD"
fi
chmod 755 "$PAYLOAD"

CLEAN=$TMP/clean-path
mkdir -p "$CLEAN"
ORIG_PATH=$PATH
for tool in sha256sum shasum tar gzip dirname cp mkdir mv chmod awk uname rm mktemp cat find wc sort; do
  if command -v "$tool" >/dev/null 2>&1; then
    ln -s "$(command -v "$tool")" "$CLEAN/$tool"
  fi
done

good=$TMP/good
build_release "$good" "$PAYLOAD"
printf '%s\n' 'not a signature, but present' > "$good/SHA256SUMS.minisig"

spaced="$TMP/dir with spaces/bin"
data="$TMP/data with spaces"
set +e
out=$(
  PATH="$CLEAN" \
  HOME="$TMP/home-ok" \
  XDG_DATA_HOME="$data" \
  GITCKO_DOWNLOAD_URL="$good" \
  GITCKO_INSTALL_DIR="$spaced" \
  "$INSTALL" 2>"$TMP/ok.err"
)
code=$?
set -e
[ "$code" = 0 ] || fail "good checksum should install: $(cat "$TMP/ok.err")"
printf '%s\n' "$out" | awk 'index($0, "installed:") == 1 { found = 1 } END { exit !found }' || fail "missing installed line"
grep -q "signature: not checked" "$TMP/ok.err" || fail "should say the signature was not checked"
if ! "$spaced/gitcko" | grep -q gitcko-test-ok; then
  "$spaced/gitcko" --version >/dev/null || fail "installed binary did not run"
fi
grep -q '^method=script$' "$spaced/gitcko-install.txt" || fail "receipt missing"
printf 'ok install with spaces, checksum only\n'

bad=$TMP/bad
mkdir -p "$bad"
cp -- "$good/SHA256SUMS" "$good/SHA256SUMS.minisig" "$bad/"
cp -- "$good/$ARCHIVE" "$bad/$ARCHIVE"
python3 - "$bad/$ARCHIVE" <<'PY'
import sys
p = sys.argv[1]
b = bytearray(open(p, "rb").read())
b[-1] ^= 0x01
open(p, "wb").write(b)
PY
kept="$TMP/keep with spaces/bin"
mkdir -p "$kept"
printf '%s\n' old-binary > "$kept/gitcko"
set +e
PATH="$CLEAN" HOME="$TMP/home-bad" GITCKO_DOWNLOAD_URL="$bad" GITCKO_INSTALL_DIR="$kept" "$INSTALL" >"$TMP/bad.out" 2>"$TMP/bad.err"
code=$?
set -e
[ "$code" != 0 ] || fail "checksum mismatch must fail"
grep -q "checksum mismatch" "$TMP/bad.err" || fail "expected checksum mismatch"
printf '%s\n' old-binary | cmp - "$kept/gitcko" || fail "old binary was replaced"
printf 'ok checksum mismatch leaves the old binary\n'

empty=$TMP/empty
build_release "$empty" "$PAYLOAD"
: > "$empty/SHA256SUMS.minisig"
fresh="$TMP/empty-dest/bin"
set +e
PATH="$CLEAN" HOME="$TMP/home-empty" GITCKO_DOWNLOAD_URL="$empty" GITCKO_INSTALL_DIR="$fresh" "$INSTALL" >"$TMP/empty.out" 2>"$TMP/empty.err"
code=$?
set -e
[ "$code" != 0 ] || fail "empty signature must fail"
grep -q "signature missing" "$TMP/empty.err" || fail "expected signature missing"
assert_absent "$fresh/gitcko" "empty signature"
printf 'ok empty signature refused\n'

failing=$TMP/with-fail
build_release "$failing" "$PAYLOAD"
printf '%s\n' dummy > "$failing/SHA256SUMS.minisig"
FAILBIN=$TMP/failbin
mkdir -p "$FAILBIN"
cat > "$FAILBIN/minisign" <<'EOF'
#!/bin/sh
exit 1
EOF
chmod 755 "$FAILBIN/minisign"
dest_fail="$TMP/fail-dest/bin"
set +e
PATH="$FAILBIN:$CLEAN" HOME="$TMP/home-fail" GITCKO_DOWNLOAD_URL="$failing" GITCKO_INSTALL_DIR="$dest_fail" "$INSTALL" >"$TMP/fail.out" 2>"$TMP/fail.err"
code=$?
set -e
[ "$code" != 0 ] || fail "failing minisign must refuse"
grep -q "signature rejected" "$TMP/fail.err" || fail "expected signature rejected"
assert_absent "$dest_fail/gitcko" "failing minisign"
printf 'ok failing minisign refuses\n'

okbin=$TMP/okbin
mkdir -p "$okbin"
cat > "$okbin/minisign" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod 755 "$okbin/minisign"
dest_ok="$TMP/fake-ok with spaces/bin"
set +e
PATH="$okbin:$CLEAN" HOME="$TMP/home-fake-ok" GITCKO_DOWNLOAD_URL="$failing" GITCKO_INSTALL_DIR="$dest_ok" "$INSTALL" >"$TMP/fake-ok.out" 2>"$TMP/fake-ok.err"
code=$?
set -e
[ "$code" = 0 ] || fail "minisign exit 0 should install: $(cat "$TMP/fake-ok.err")"
test -x "$dest_ok/gitcko" || fail "fake verifier did not install"
printf 'ok minisign exit 0 installs\n'

set +e
PATH="$CLEAN" HOME="$TMP/home-http" GITCKO_DOWNLOAD_URL="http://example.com/canary_update_secret" GITCKO_INSTALL_DIR="$TMP/http-dest/bin" "$INSTALL" >"$TMP/http.out" 2>"$TMP/http.err"
code=$?
set -e
[ "$code" != 0 ] || fail "public http must be refused"
grep -q "download refused" "$TMP/http.err" || fail "expected download refused"
if grep -q canary_update_secret "$TMP/http.err"; then
  fail "error echoed the url"
fi
assert_absent "$TMP/http-dest/bin/gitcko" "public http"
printf 'ok public http refused\n'

file_dest="$TMP/file dest/bin"
file_url="file://${good}"
set +e
PATH="$CLEAN" HOME="$TMP/home-file" XDG_DATA_HOME="$TMP/file-data" GITCKO_DOWNLOAD_URL="$file_url" GITCKO_INSTALL_DIR="$file_dest" "$INSTALL" >"$TMP/file.out" 2>"$TMP/file.err"
code=$?
set -e
[ "$code" = 0 ] || fail "file url should install: $(cat "$TMP/file.err")"
test -x "$file_dest/gitcko" || fail "file url did not install"
printf 'ok file url\n'

if [ "${GITCKO_REAL_MINISIGN:-}" = 1 ]; then
  command -v minisign >/dev/null 2>&1 || fail "GITCKO_REAL_MINISIGN set but minisign is not on PATH"
  real=$TMP/real
  build_release "$real" "$PAYLOAD"
  minisign -S -W -s "$HERE/testdata/minisign.secret" -m "$real/SHA256SUMS" -x "$real/SHA256SUMS.minisig" -t "gitcko test" -q
  real_dest="$TMP/real with spaces/bin"
  set +e
  PATH="$ORIG_PATH" HOME="$TMP/home-real" XDG_DATA_HOME="$TMP/real-data" GITCKO_DOWNLOAD_URL="$real" GITCKO_INSTALL_DIR="$real_dest" "$INSTALL" >"$TMP/real.out" 2>"$TMP/real.err"
  code=$?
  set -e
  [ "$code" = 0 ] || fail "real signature should install: $(cat "$TMP/real.err")"
  test -x "$real_dest/gitcko" || fail "real signature did not install"
  other=$TMP/other-key
  mkdir -p "$other"
  minisign -G -f -W -p "$other/pub" -s "$other/key" >/dev/null
  wrong=$TMP/wrong
  build_release "$wrong" "$PAYLOAD"
  minisign -S -W -s "$other/key" -m "$wrong/SHA256SUMS" -x "$wrong/SHA256SUMS.minisig" -t "other" -q
  wrong_dest="$TMP/wrong-dest/bin"
  mkdir -p "$wrong_dest"
  printf '%s\n' previous > "$wrong_dest/gitcko"
  set +e
  PATH="$ORIG_PATH" HOME="$TMP/home-wrong" GITCKO_DOWNLOAD_URL="$wrong" GITCKO_INSTALL_DIR="$wrong_dest" "$INSTALL" >"$TMP/wrong.out" 2>"$TMP/wrong.err"
  code=$?
  set -e
  [ "$code" != 0 ] || fail "wrong key must be refused"
  grep -q "signature rejected" "$TMP/wrong.err" || fail "expected signature rejected for the other key"
  printf '%s\n' previous | cmp - "$wrong_dest/gitcko" || fail "wrong key replaced the binary"
  tamper=$TMP/tamper
  mkdir -p "$tamper"
  cp -- "$real/SHA256SUMS" "$real/SHA256SUMS.minisig" "$tamper/"
  cp -- "$real/$ARCHIVE" "$tamper/$ARCHIVE"
  python3 - "$tamper/$ARCHIVE" <<'PY'
import sys
p = sys.argv[1]
b = bytearray(open(p, "rb").read())
b[0] ^= 0x01
open(p, "wb").write(b)
PY
  tamper_dest="$TMP/tamper-dest/bin"
  mkdir -p "$tamper_dest"
  printf '%s\n' previous > "$tamper_dest/gitcko"
  set +e
  PATH="$ORIG_PATH" HOME="$TMP/home-tamper" GITCKO_DOWNLOAD_URL="$tamper" GITCKO_INSTALL_DIR="$tamper_dest" "$INSTALL" >"$TMP/tamper.out" 2>"$TMP/tamper.err"
  code=$?
  set -e
  [ "$code" != 0 ] || fail "tampered archive must be refused"
  printf '%s\n' previous | cmp - "$tamper_dest/gitcko" || fail "tampered archive replaced the binary"
  printf 'ok real minisign accepts the test key and refuses the other key\n'
fi

printf 'install.sh tests passed\n'
