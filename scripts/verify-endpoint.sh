#!/bin/bash
# Verify the "must run inside the CNB workspace" premise.
#
# Run this BOTH from your public machine and from inside a CNB workspace and
# compare. The endpoint only answers when the request originates inside CNB:
#
#   outside (any IP)  -> 412 {"errcode":10412,"errmsg":"Blocked by network policy."}
#   inside workspace  -> 200 + SSE stream
#
# Usage:
#   CNB_TOKEN=xxx CNB_REPO_SLUG=org/repo bash verify-endpoint.sh
set -uo pipefail

API="${CNB_API:-https://api.cnb.cool}"
REPO="${CNB_REPO_SLUG:-}"
TOKEN="${CNB_TOKEN:-}"

if [ -z "$REPO" ] || [ -z "$TOKEN" ]; then
  echo "need CNB_REPO_SLUG and CNB_TOKEN in the environment" >&2
  exit 1
fi

probe() {
  local label="$1"; shift
  local code
  code=$(curl -sS -o /tmp/probe.out -w '%{http_code}' --max-time 30 "$@" 2>/dev/null || echo "000")
  # Pure-bash truncation: no head/tr/cut/paste, so a bare shell still works.
  local body=""
  if [ -r /tmp/probe.out ]; then
    body=$(</tmp/probe.out)
    body="${body:0:160}"
    body="${body//$'\n'/ }"
  fi
  printf '%-34s -> %s  %s\n' "$label" "$code" "$body"
}

echo "=== api host: $API"
echo "=== repo    : $REPO"
echo

echo "--- 1. auth layer: is the token accepted? ---"
probe "chat, no token" \
  -X POST "$API/$REPO/-/ai/chat/completions" \
  -H "Content-Type: application/json" \
  -d '{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"hi"}]}'
probe "chat, bogus token" \
  -X POST "$API/$REPO/-/ai/chat/completions" \
  -H "Authorization: Bearer sk-bogus" \
  -H "Content-Type: application/json" \
  -d '{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"hi"}]}'
probe "chat, REAL token" \
  -X POST "$API/$REPO/-/ai/chat/completions" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"hi"}]}'
echo "   401 = not authenticated;  412 = authenticated but IP blocked;  200 = in-network"
echo

echo "--- 2. is the block global or AI-only? ---"
probe "GET /user/repos"        "$API/user/repos" -H "Authorization: Bearer $TOKEN"
probe "GET /workspace/list"    "$API/workspace/list?slug=$REPO" -H "Authorization: Bearer $TOKEN"
probe "GET /$REPO"             "$API/$REPO" -H "Authorization: Bearer $TOKEN"
echo "   all 412 => the whole api host is IP-gated, not just the AI route"
echo

echo "--- 3. does /v1/models exist upstream? ---"
probe "GET /-/ai/models"       "$API/$REPO/-/ai/models" -H "Authorization: Bearer $TOKEN"
probe "GET /-/ai/v1/models"    "$API/$REPO/-/ai/v1/models" -H "Authorization: Bearer $TOKEN"
echo "   404 => no such route. CNB only defines /{repo}/-/ai/chat/completions;"
echo "          /v1/models must be served locally by CYarpServer (ProxyModels)."
echo

echo "--- 4. streaming chat (the real call) ---"
echo "curl -sS -N \"$API/$REPO/-/ai/chat/completions\" \\"
echo "  -H \"Authorization: Bearer \$CNB_TOKEN\" -H 'Content-Type: application/json' \\"
echo "  -d '{\"model\":\"deepseek-v4-flash\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}],\"stream\":true}'"
echo
echo "--- first SSE frame:"
sse=$(curl -sS -N --max-time 60 "$API/$REPO/-/ai/chat/completions" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"hi"}],"stream":true}' \
  2>/dev/null)
printf '%s\n' "${sse:0:400}"
if [ -z "$sse" ]; then
  echo "(empty response - expected when running outside the CNB network)"
fi
