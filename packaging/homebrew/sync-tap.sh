#!/usr/bin/env sh
# Refresh the Homebrew tap formula from a published release.
#
# The formula carries one checksum per platform, so it can only be written after the archives
# exist. This script downloads each sidecar and rewrites `packaging/homebrew/Formula/spavn.rb`,
# then optionally commits the result into the tap checkout — one source of truth, no hand-edited
# hashes, and no version ever pointing at an archive that was not published.
#
#   sh packaging/homebrew/sync-tap.sh 0.2.0 [path-to-tap-checkout]
#
# Without a tap path it updates the formula in this repo only, which is what CI does.
set -eu

REPO="tugayoktayokay/spavn-cli"
HERE="$(cd "$(dirname "$0")" && pwd)"
FORMULA="$HERE/Formula/spavn.rb"
TAP_DIR="${2:-$HERE/../../homebrew-tap}"

if [ $# -lt 1 ]; then
  echo "usage: sh packaging/homebrew/sync-tap.sh <version-without-v> [tap-checkout]" >&2
  exit 2
fi
VERSION="${1#v}"

# Prints the checksum, or nothing when that target was not built for this tag.
sha_for() {
  _target="$1"
  _url="https://github.com/${REPO}/releases/download/v${VERSION}/spavn-${_target}.tar.gz.sha256"
  if ! _sum="$(curl -fsSL "$_url" 2>/dev/null | awk '{print $1}')"; then
    return 1
  fi
  case "$_sum" in
    ????????????????????????????????????????????????????????????????) printf '%s' "$_sum" ;;
    *) return 1 ;;
  esac
}

# Hard requirement: without these three the formula cannot describe a real install.
require_sha() {
  if ! _s="$(sha_for "$1")"; then
    echo "Error: v${VERSION} has no archive for $1 — refusing to write a formula that cannot install." >&2
    exit 1
  fi
  printf '%s' "$_s"
}

echo "Fetching checksums for v${VERSION}..."
MAC_ARM="$(require_sha aarch64-apple-darwin)"
MAC_X86="$(require_sha x86_64-apple-darwin)"
LINUX_X86="$(require_sha x86_64-unknown-linux-gnu)"
LINUX_ARM="$(sha_for aarch64-unknown-linux-gnu || true)"

if [ -n "$LINUX_ARM" ]; then
  # Written to a file rather than passed with `-v`: BSD awk rejects a newline inside a -v value,
  # which would have made this branch fail only on macOS.
  ARM_FILE="$HERE/.linux-arm.$$"
  {
    printf '    on_arm do\n'
    printf '      url "https://github.com/%s/releases/download/v#{version}/spavn-aarch64-unknown-linux-gnu.tar.gz"\n' "$REPO"
    printf '      sha256 "%s"\n' "$LINUX_ARM"
    printf '    end\n'
  } > "$ARM_FILE"
  echo "  linux arm64: $LINUX_ARM"
else
  ARM_FILE=/dev/null
  echo "  linux arm64: not published for v${VERSION} — block omitted"
fi
trap 'rm -f "$ARM_FILE" "$FORMULA.new.$$" 2>/dev/null || true' EXIT

# One pass: bump the version, repoint every download URL at this tag, refresh each checksum from
# the `url` line above it, and rewrite the linux-arm region between its two markers.
_out="$FORMULA.new.$$"
awk -v ver="$VERSION" -v arm="$MAC_ARM" -v x86="$MAC_X86" -v lx="$LINUX_X86" -v armfile="$ARM_FILE" '
  /^  version "/ { print "  version \"" ver "\""; next }

  # The region check has to come first: the url/sha256 rules below would otherwise print the
  # stale block while `skip` is set, leaving orphaned lines inside the markers.
  /^ *# END linux-arm/ { skip = 0; print; next }
  skip { next }

  /^ *# BEGIN linux-arm/ {
    print
    while ((getline line < armfile) > 0) print line
    close(armfile)
    skip = 1
    next
  }

  /^ *url "/ {
    # Leave the `v#{version}` idiom alone: rewriting it to a literal tag would freeze every future
    # release on this version. Only a hardcoded tag gets repointed.
    if ($0 !~ /#\{version\}/) sub(/releases\/download\/v[^\/]*\//, "releases/download/v" ver "/")
    print
    if ($0 ~ /aarch64-apple-darwin/) want = arm
    else if ($0 ~ /x86_64-apple-darwin/) want = x86
    else if ($0 ~ /x86_64-unknown-linux-gnu/) want = lx
    else want = ""
    next
  }

  /^ *sha256 "/ {
    if (want != "") { sub(/"[0-9a-f]+"/, "\"" want "\""); want = "" }
    print
    next
  }

  { print }
' "$FORMULA" > "$_out"
mv -f "$_out" "$FORMULA"
echo "✅ Updated $FORMULA to v${VERSION}"

if [ -d "$TAP_DIR/Formula" ]; then
  cp "$FORMULA" "$TAP_DIR/Formula/spavn.rb"
  (
    cd "$TAP_DIR"
    git add Formula/spavn.rb
    git commit -q -m "spavn ${VERSION}" || echo "  (formula already at ${VERSION})"
    git push -q origin HEAD && echo "✅ Pushed tap: $TAP_DIR"
  )
else
  echo "ℹ️  No tap checkout at $TAP_DIR — copy Formula/spavn.rb into homebrew-tap/Formula/."
fi
