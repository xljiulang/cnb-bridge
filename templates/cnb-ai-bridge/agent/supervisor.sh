#!/usr/bin/env bash
set -u

BASE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOG_DIR="$BASE_DIR/.cnb-ai-bridge"
LOG_FILE="$LOG_DIR/agent.log"
mkdir -p "$LOG_DIR"

while true; do
  node "$BASE_DIR/agent/agent.mjs" --port 11233 >>"$LOG_FILE" 2>&1
  rc=$?
  echo "$(date -Is) agent exited rc=$rc; restarting in 1s" >>"$LOG_FILE"
  sleep 1
done
