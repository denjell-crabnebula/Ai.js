#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
# SPDX-License-Identifier: Apache-2.0

# Build the wasm module and generate JavaScript bindings for Node and the browser.
#
#   rust/agent-protocol-wasm/examples/build.sh            # release build
#   PROFILE=debug rust/agent-protocol-wasm/examples/build.sh
#
# Output:
#   rust/agent-protocol-wasm/pkg/node/   CommonJS bindings for Node (require)
#   rust/agent-protocol-wasm/pkg/web/    ES module bindings for browsers (import)
#
# Requires the wasm32 target and the wasm-bindgen CLI matching the
# wasm-bindgen crate version pinned in Cargo.toml:
#   rustup target add wasm32-unknown-unknown
#   cargo install wasm-bindgen-cli --version 0.2.128
set -euo pipefail

CRATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT_DIR="$(cd "$CRATE_DIR/../.." && pwd)"
PROFILE="${PROFILE:-release}"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT_DIR/target}"
WASM="$TARGET_DIR/wasm32-unknown-unknown/$PROFILE/agent_protocol_wasm.wasm"

cd "$ROOT_DIR"
if [ "$PROFILE" = "release" ]; then
  cargo build -p agent-protocol-wasm --target wasm32-unknown-unknown --release
else
  cargo build -p agent-protocol-wasm --target wasm32-unknown-unknown
fi

rm -rf "$CRATE_DIR/pkg"
wasm-bindgen --target nodejs --out-dir "$CRATE_DIR/pkg/node" "$WASM"
wasm-bindgen --target web --out-dir "$CRATE_DIR/pkg/web" "$WASM"

echo "node bindings: $CRATE_DIR/pkg/node"
echo "web bindings:  $CRATE_DIR/pkg/web"
