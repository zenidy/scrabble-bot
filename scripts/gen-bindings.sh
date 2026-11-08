#!/usr/bin/env bash
# Regenerate src/net/module_bindings from the event repo at the pinned commit.
# Needs the spacetime CLI at the same version as the spacetimedb-sdk crate,
# plus node/npm (the module is TypeScript).
#   EVENT_REPO=../scrabblebot-event scripts/gen-bindings.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
EVENT_REPO="${EVENT_REPO:-$ROOT/../scrabblebot-event}"
COMMIT="$(tr -d '[:space:]' < "$ROOT/EVENT_REPO_COMMIT")"
SDK_VERSION="$(grep -E '^spacetimedb-sdk' "$ROOT/Cargo.toml" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')"

command -v spacetime >/dev/null || { echo "spacetime CLI not found (install version $SDK_VERSION)"; exit 1; }
CLI_VERSION="$(spacetime --version | grep -oE 'tool version [0-9.]+' | awk '{print $3}')"
if [ "$CLI_VERSION" != "$SDK_VERSION" ]; then
  echo "spacetime CLI is $CLI_VERSION but spacetimedb-sdk is $SDK_VERSION; align them first"; exit 1
fi

if [ ! -d "$EVENT_REPO/.git" ]; then
  git clone https://github.com/clockworklabs/scrabblebot.git "$EVENT_REPO"
fi
git -C "$EVENT_REPO" fetch --quiet origin
git -C "$EVENT_REPO" checkout --quiet "$COMMIT"
(cd "$EVENT_REPO/spacetimedb" && npm install --no-audit --no-fund)

rm -rf "$ROOT/src/net/module_bindings"
spacetime generate --lang rust --out-dir "$ROOT/src/net/module_bindings" --module-path "$EVENT_REPO/spacetimedb" -y

if ! cmp -s "$EVENT_REPO/spacetimedb/wordlist.txt" "$ROOT/data/wordlist.txt"; then
  echo "WARNING: the wordlist changed upstream; update data/wordlist.txt and WORDLIST_SHA256 in src/game/dict.rs"
fi
echo "bindings regenerated from $COMMIT"
