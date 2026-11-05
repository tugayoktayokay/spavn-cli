#!/usr/bin/env sh
# Publish the docs site to spavn.dev.
#
# `docs/index.html` is the whole site — one file, no build step — so publishing is a copy plus a
# checksum. The script refuses to claim success on the strength of `curl` alone: it hashes both
# ends and fails loudly when the bytes that landed on the server are not the bytes in the repo.
#
#   sh packaging/site/deploy-docs.sh [host]
#
# The host defaults to the one holding the spavn.dev vhost. Installing under /var/www needs root,
# and the remote account is expected to have passwordless sudo (as it does today); the script
# reports that requirement instead of hanging on a prompt.
set -eu

HOST="${1:-telya-server}"
REMOTE_ROOT="/var/www/spavn.dev"
DOCS_URL="https://spavn.dev/docs/"
HERE="$(cd "$(dirname "$0")" && pwd)"
LOCAL="$HERE/../../docs/index.html"

if [ ! -f "$LOCAL" ]; then
  echo "deploy-docs: missing $LOCAL" >&2
  exit 1
fi

# sha256 of a local file, on either platform's tooling.
local_sha() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

# Hard requirement: a sudo pasted prompt would hang a non-interactive deploy, so check first.
if ! ssh -o ConnectTimeout=10 "$HOST" "sudo -n true" 2>/dev/null; then
  echo "deploy-docs: $HOST needs passwordless sudo to write $REMOTE_ROOT" >&2
  exit 1
fi

WANT="$(local_sha "$LOCAL")"
echo "deploy-docs: $LOCAL -> $HOST:$REMOTE_ROOT/docs/ ($WANT)"

# Staging in /tmp first keeps a half-written index.html from ever being served.
rsync -az "$LOCAL" "$HOST:/tmp/spavn-docs-index.html"
ssh -o ConnectTimeout=10 "$HOST" "
  sudo install -d -o www-data -g www-data -m 755 '$REMOTE_ROOT/docs' &&
  sudo install -o www-data -g www-data -m 644 /tmp/spavn-docs-index.html '$REMOTE_ROOT/docs/index.html' &&
  rm -f /tmp/spavn-docs-index.html &&
  sha256sum '$REMOTE_ROOT/docs/index.html' | awk '{print \$1}'
" > /tmp/spavn-docs-remote-sha

GOT="$(cat /tmp/spavn-docs-remote-sha)"
rm -f /tmp/spavn-docs-remote-sha

if [ "$WANT" != "$GOT" ]; then
  echo "deploy-docs: checksum mismatch (want $WANT, got $GOT)" >&2
  exit 1
fi

# The server is authoritative; a mismatch here means the vhost is not serving this directory.
LIVE="$(curl -fsSL "$DOCS_URL" | { command -v shasum >/dev/null 2>&1 && shasum -a 256 || sha256sum; } | awk '{print $1}')"
if [ "$WANT" != "$LIVE" ]; then
  echo "deploy-docs: $DOCS_URL served $LIVE, expected $WANT" >&2
  exit 1
fi

echo "deploy-docs: live at $DOCS_URL (verified)"
