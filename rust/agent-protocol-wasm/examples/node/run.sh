#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
# SPDX-License-Identifier: Apache-2.0

# One-command demo: build the wasm bindings, start the Rust A4P server with
# the Ed25519 signature method, run the Node User Authorizer example, stop
# the server.
#
#   rust/agent-protocol-wasm/examples/node/run.sh
#
# Environment:
#   A4P_PORT   port for the temporary server (default 18961)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$HERE/../../../.." && pwd)"
PORT="${A4P_PORT:-18961}"
WORK="$(mktemp -d)"
TRUST_FILE="$WORK/ed25519_trusted_server_keys.json"

cd "$ROOT_DIR"
"$HERE/../build.sh"
cargo build -p a4p --example run_ed25519_authorization_server

target/debug/examples/run_ed25519_authorization_server \
  --port "$PORT" --trusted-keys-output "$TRUST_FILE" >"$WORK/server.log" 2>&1 &
SERVER_PID=$!
cleanup() {
  kill "$SERVER_PID" 2>/dev/null || true
  wait "$SERVER_PID" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

for _ in $(seq 1 50); do
  if [ -s "$TRUST_FILE" ] && curl -sf -o /dev/null -X POST -H 'content-type: application/json' \
       -d '{}' "http://127.0.0.1:$PORT/a4p/v1/intent-tokens/verify" 2>/dev/null; then
    break
  fi
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "server exited early:"; cat "$WORK/server.log"; exit 1
  fi
  sleep 0.2
done

A4P_SERVER_BASE_URL="http://127.0.0.1:$PORT" A4P_TRUSTED_KEYS="$TRUST_FILE" \
  node "$HERE/authorize.cjs"
