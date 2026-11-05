#!/usr/bin/env sh
set -eu

REPO="tugayoktayokay/spavn-cli"
INSTALL_DIR="${SPAVN_INSTALL_DIR:-$HOME/.local/bin}"

# Replacing a binary in place (cp over the same inode) leaves a stale code-signature
# in the macOS kernel cache and amfid SIGKILLs the process on execve. Always stage a
# fresh inode, ad-hoc sign it, then atomically rename it into place, and prove the
# installed path actually runs before claiming success.
install_binary() {
  _src="$1"
  _tmp="$INSTALL_DIR/.spavn.tmp.$$"
  cp "$_src" "$_tmp"
  chmod +x "$_tmp"
  if [ "$(uname -s)" = "Darwin" ] && command -v codesign >/dev/null 2>&1; then
    codesign -s - -f "$_tmp" >/dev/null 2>&1 || true
  fi
  xattr -c "$_tmp" >/dev/null 2>&1 || true
  mv -f "$_tmp" "$INSTALL_DIR/spavn"
  ln -sf "$INSTALL_DIR/spavn" "$INSTALL_DIR/spvn"
  if ! "$INSTALL_DIR/spavn" --version >/dev/null 2>&1; then
    echo "Error: $INSTALL_DIR/spavn does not run (killed on execve)." >&2
    echo "       The staged signature is unusable. Re-run this installer, or move the" >&2
    echo "       binary to a new path with: cp $INSTALL_DIR/spavn /tmp/spavn && /tmp/spavn --version" >&2
    exit 1
  fi
}

LOCAL_BIN=""
case "${1:-}" in
  --local)   LOCAL_BIN="${2:-target/release/spavn}" ;;
  --local=*) LOCAL_BIN="${1#--local=}" ;;
esac

echo "⚡ Installing SPAVN — Native Rust Terminal Coding Agent..."
mkdir -p "$INSTALL_DIR"

if [ -n "$LOCAL_BIN" ]; then
  if [ ! -x "$LOCAL_BIN" ]; then
    echo "Error: local binary not found or not executable: $LOCAL_BIN" >&2
    exit 1
  fi
  install_binary "$LOCAL_BIN"
  echo "✅ SPAVN installed to $INSTALL_DIR/spavn (and alias 'spvn')"
  echo "🚀 Run 'spavn' to start!"
  exit 0
fi

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Darwin)
    case "$ARCH" in
      arm64|aarch64) TARGET="aarch64-apple-darwin" ;;
      x86_64)        TARGET="x86_64-apple-darwin" ;;
      *) echo "Unsupported macOS architecture: $ARCH" >&2; exit 1 ;;
    esac
    ;;
  Linux)
    case "$ARCH" in
      x86_64)        TARGET="x86_64-unknown-linux-gnu" ;;
      arm64|aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
      *) echo "Unsupported Linux architecture: $ARCH" >&2; exit 1 ;;
    esac
    ;;
  *)
    echo "Unsupported OS: $OS (use cargo install --git https://github.com/$REPO)" >&2
    exit 1
    ;;
esac

TARBALL_URL="https://github.com/${REPO}/releases/latest/download/spavn-${TARGET}.tar.gz"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

if curl -fsSL "$TARBALL_URL" -o "$TMP_DIR/spavn.tar.gz" 2>/dev/null; then
  tar -xzf "$TMP_DIR/spavn.tar.gz" -C "$TMP_DIR"
  install_binary "$TMP_DIR/spavn"
elif command -v cargo >/dev/null 2>&1; then
  echo "📦 Building from source with Cargo..."
  cargo install --git "https://github.com/${REPO}.git" --locked --root "$HOME/.local"
  # Cargo always drops the binary in $HOME/.local/bin; route it through install_binary so the
  # staged inode is signed and probed even when SPAVN_INSTALL_DIR points somewhere else.
  install_binary "$HOME/.local/bin/spavn"
else
  echo "Error: Prebuilt binary not found for $TARGET and Cargo is not installed." >&2
  exit 1
fi

echo "✅ SPAVN installed to $INSTALL_DIR/spavn (and alias 'spvn')"
echo "🚀 Run 'spavn' to start!"
