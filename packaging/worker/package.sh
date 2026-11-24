#!/usr/bin/env bash
# Build GridCraft's engine for a Cloudflare Worker and zip it:  $DIST/gridcraft-worker-<version>.zip
#
# The zip holds gridcraft-worker-<version>/ with wasm-bindgen's `--target web` output
# (apps/gridcraft-worker). A Worker imports gridcraft_worker_bg.wasm, which it gets as a compiled
# WebAssembly.Module, and hands it to `initSync`. Needs the wasm32 target and the wasm-bindgen CLI
# at the version Cargo.lock pins:
#   rustup target add wasm32-unknown-unknown
#   cargo install wasm-bindgen-cli --version <Cargo.lock's wasm-bindgen> --locked
set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"

WANT=$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/"/, "", $3); print $3; exit }' "$ROOT/Cargo.lock")
command -v wasm-bindgen >/dev/null || { echo "error: wasm-bindgen not found (cargo install wasm-bindgen-cli --version $WANT --locked)" >&2; exit 1; }
HAVE=$(wasm-bindgen --version | awk '{print $2}')
if [ "$HAVE" != "$WANT" ]; then
  echo "error: wasm-bindgen $HAVE, Cargo.lock pins $WANT (cargo install wasm-bindgen-cli --version $WANT --locked)" >&2
  exit 1
fi

# The web profile: fat LTO and opt-level 3, like the browser build.
cargo build -p gridcraft-worker --lib --target wasm32-unknown-unknown --profile web --locked

NAME="gridcraft-worker-$VERSION"
WORK="$CARGO_TARGET_DIR/worker-package"
rm -rf "$WORK"
mkdir -p "$WORK/$NAME"
wasm-bindgen --target web --out-dir "$WORK/$NAME" "$CARGO_TARGET_DIR/wasm32-unknown-unknown/web/gridcraft_worker.wasm"
copy_docs "$WORK/$NAME"
rm -f "$DIST/$NAME.zip"
(cd "$WORK" && zip -qr9 "$DIST/$NAME.zip" "$NAME")
echo "wrote $DIST/$NAME.zip"
ls -lh "$DIST/$NAME.zip" "$WORK/$NAME/"*.wasm
