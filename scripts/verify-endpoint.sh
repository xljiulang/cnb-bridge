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

# Same as probe(), but prints the whole body and keeps it in a file. Used for the charge endpoints,
# whose interesting fields (credit_in_milli / freeze_credit_in_milli) sit near the end and would be
# cut off by the 160 character truncation above.
#   usage: probe_full <label> <out-file> <curl args...>
probe_full() {
  local label="$1"; local out="$2"; shift 2
  local code
  code=$(curl -sS -o "$out" -w '%{http_code}' --max-time 30 "$@" 2>/dev/null || echo "000")
  printf '%-34s -> %s\n' "$label" "$code"
  if [ -r "$out" ]; then
    printf '    %s\n' "$(<"$out")"
  fi
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

echo
echo "--- 5. AI quota of the organization (what CYarpServer probes through the tunnel) ---"
# CYarpServer reads these two to learn how many credits the organization has left:
#   remaining = quota.credit_in_milli.total - (volume.credit_in_milli + volume.freeze_credit_in_milli)
# They need the group-resource:r permission on the token, so run this INSIDE the workspace: the
# workspace token does carry it (verified 2026-09: both answer 200 with a pipeline CNB_TOKEN).
# 200 = the quota routing will work; 401/403 = the server will read no quota and the bare
# /v1/chat/completions route falls back to a uniform random pick (still fully usable).
# The organization is the first segment of the repo slug.
ORG="${REPO%%/*}"
probe_full "GET /$ORG/-/charge/quota" /tmp/cyarp-quota.out \
  "$API/$ORG/-/charge/quota" -H "Authorization: Bearer $TOKEN" -H "Accept: application/vnd.cnb.api+json"
probe_full "GET /$ORG/-/charge/volume" /tmp/cyarp-volume.out \
  "$API/$ORG/-/charge/volume" -H "Authorization: Bearer $TOKEN" -H "Accept: application/vnd.cnb.api+json"
echo "   200 => quota routing works;  401/403 => the token lacks group-resource:r, routing stays random"

# Why the full bodies matter: credit_in_milli sits near the end of both responses (quota lists every
# core-second allowance first), so the 160 character truncation of probe() would hide the one field
# the server actually reads. Print the resulting number too, when jq is available.
echo "   the fields CYarpServer reads: quota.credit_in_milli.total - (volume.credit_in_milli + volume.freeze_credit_in_milli)"
if command -v jq >/dev/null 2>&1; then
  total=$(jq -r '.credit_in_milli.total // empty' /tmp/cyarp-quota.out 2>/dev/null || true)
  used=$(jq -r '.credit_in_milli // empty' /tmp/cyarp-volume.out 2>/dev/null || true)
  frozen=$(jq -r '.freeze_credit_in_milli // 0' /tmp/cyarp-volume.out 2>/dev/null || true)
  if [ -n "$total" ] && [ -n "$used" ]; then
    # Integer arithmetic only (a bare CNB shell has no bc/awk). Split off the sign so that an
    # overspent organization prints "-0.500" instead of the nonsense a truncated division and a
    # negative remainder would produce.
    milli=$((total - used - ${frozen:-0}))
    if [ "$milli" -lt 0 ]; then
      sign="-"; magnitude=$((0 - milli))
    else
      sign=""; magnitude=$milli
    fi
    printf '   remaining = %s - (%s + %s) = %s milli-credits = %s%d.%03d credits\n' \
      "$total" "$used" "${frozen:-0}" "$milli" "$sign" "$((magnitude / 1000))" "$((magnitude % 1000))"
  else
    echo "   (could not read credit_in_milli from these responses - see the bodies above)"
  fi
else
  echo "   (jq not found: subtract the two credit_in_milli numbers above)"
fi
