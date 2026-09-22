#!/bin/bash
# CNB workspace boot script: run the Native AOT CYarpClient as a long-lived tunnel to
# your public CYarpServer, forwarding the workspace-internal CNB AI endpoint.
# Invoked by the `$: vscode` stage in .cnb.yml.
#
# Layout contract: this script and the client binary live in the same directory, i.e.
#   <payload>/cyarp-start.sh
#   <payload>/CYarpClient
# where <payload> is the CNB repository root (the contents of artifacts/client/ in the tool
# repository). This file lives next to the client project (src/CYarpClient/) and is copied
# into artifacts/client/ by scripts/publish.ps1.
#
# Injected by the CNB pipeline (do not set these yourself):
#   CNB_TOKEN      temporary CNB credential, auto-issued per pipeline run
#   CNB_REPO_SLUG  built-in org/repo
# You must provide (via .cnb.yml env: or a secrets repo import):
#   CYARP_SERVER_URI  e.g. http://ai.example.com  (use https:// once the server has TLS)
# Optional:
#   CYARP_RUN_DIR     where cyarp.log is written, default /workspace/cyarp
#   CYARP_BIN         client binary path, default <script dir>/cyarp/CYarpClient
#
# The client does not carry any public API key: only the CNB token is sent, in the handshake.
set -euo pipefail

SERVER_URI="${CYARP_SERVER_URI:?CYARP_SERVER_URI is required, e.g. http://ai.example.com}"
REPO="${CNB_REPO_SLUG:?CNB_REPO_SLUG is missing}"
# The client id is always the repo slug (org/repo, slashes included) - it is not configurable,
# so the public path prefix can never drift from the workspace that is actually tunneling.
CLIENT_ID="$REPO"
RUN_DIR="${CYARP_RUN_DIR:-/workspace/cyarp}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="${CYARP_BIN:-$SCRIPT_DIR/CYarpClient}"

if [ -z "${CNB_TOKEN:-}" ]; then
  echo "===== CNB_TOKEN is missing, refusing to start ====="
  exit 1
fi

chmod +x "$BIN" 2>/dev/null || true
if [ ! -x "$BIN" ]; then
  echo "===== client binary not found or not executable: $BIN ====="
  exit 1
fi

mkdir -p "$RUN_DIR"
cd "$RUN_DIR"

# Idempotent hot restart (workspace reboot / stage re-run)
pkill -f "$(basename "$BIN")" 2>/dev/null || true
sleep 0.5

# Every setting travels as an environment variable, so the CNB token is never written
# to disk. Double underscore maps to the nested config sections.
#
# `env` is used instead of `export` on purpose: "X-CYarp-Client" contains a hyphen and
# is not a valid shell identifier, so `export CYarpClientOptions__ConnectHeaders__X-CYarp-Client=...`
# would fail with "not a valid identifier".
nohup env \
  "CYarpClientOptions__ServerUri=$SERVER_URI" \
  "CYarpClientOptions__TargetUri=https://api.cnb.cool/${REPO}/-/ai" \
  "CYarpClientOptions__ConnectHeaders__Authorization=Bearer ${CNB_TOKEN}" \
  "CYarpClientOptions__ConnectHeaders__X-CYarp-Client=${CLIENT_ID}" \
  "CYarpClientOptions__KeepAliveInterval=00:00:30" \
  "CYarpClientOptions__ConnectTimeout=00:00:10" \
  "$BIN" > cyarp.log 2>&1 &
CLIENT_PID=$!

echo "===== cyarp client started, pid=$CLIENT_PID ====="
echo "===== server     : $SERVER_URI ====="
echo "===== target     : https://api.cnb.cool/${REPO}/-/ai ====="
echo "===== clientId   : $CLIENT_ID ====="
echo "===== public base: <your-domain>/${CLIENT_ID}/v1/   (http:// now, https:// once TLS is enabled) ====="

# Self-check: the client logs a "connected" line once the tunnel is up. CYarp 2.0.3 logs
# in Chinese ("连接到服务器...成功"), newer builds log "Connected to server"; accept both.
for i in $(seq 1 30); do
  if grep -qE "连接到服务器|Connected to server" cyarp.log 2>/dev/null; then
    echo "===== cyarp client: tunnel established ====="
    exit 0
  fi
  if ! kill -0 "$CLIENT_PID" 2>/dev/null; then
    echo "===== cyarp client died during boot ====="
    tail -50 cyarp.log || true
    exit 1
  fi
  sleep 1
done

echo "===== cyarp client: no tunnel after 30s ====="
tail -50 cyarp.log || true
exit 1
