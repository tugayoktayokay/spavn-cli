#!/usr/bin/env sh
set -eu

REPO="tugayoktayokay/spavn-cli"
INSTALL_DIR="${SPAVN_INSTALL_DIR:-$HOME/.local/bin}"

echo "⚡ Installing SPAVN — Native Rust Terminal Coding Agent..."
mkdir -p "$INSTALL_DIR"

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
  mv "$TMP_DIR/spavn" "$INSTALL_DIR/spavn"
  chmod +x "$INSTALL_DIR/spavn"
  if [ "$OS" = "Darwin" ] && command -v codesign >/dev/null 2>&1; then
    codesign -s - -f "$INSTALL_DIR/spavn" >/dev/null 2>&1 || true
  fi
  ln -sf "$INSTALL_DIR/spavn" "$INSTALL_DIR/spvn"
elif command -v cargo >/dev/null 2>&1; then
  echo "📦 Building from source with Cargo..."
  cargo install --git "https://github.com/${REPO}.git" --locked --root "$HOME/.local"
  ln -sf "$INSTALL_DIR/spavn" "$INSTALL_DIR/spvn"
else
  echo "Error: Prebuilt binary not found for $TARGET and Cargo is not installed." >&2
  exit 1
fi

echo "✅ SPAVN installed to $INSTALL_DIR/spavn (and alias 'spvn')"
echo "🚀 Run 'spavn' to start!"
