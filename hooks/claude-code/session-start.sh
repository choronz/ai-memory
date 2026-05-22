#!/bin/sh
# Claude Code SessionStart hook.
#
# 1. Forwards the event JSON to the ai-memory server (fire-and-forget).
# 2. Synchronously fetches any pending cross-agent handoff and prints
#    it to stdout — Claude Code prepends this to the session, so the
#    next agent picks up where the previous one left off without the
#    user having to ask.
#
# Both calls are capped at sub-second timeouts so a server outage
# never blocks startup. The hook always exits 0.
SERVER="${AI_MEMORY_HOOK_URL:-http://127.0.0.1:49374}"
PAYLOAD=$(cat)
echo "$PAYLOAD" | curl -s --max-time 0.5 \
    -X POST "$SERVER/hook?event=session-start&agent=claude-code" \
    -H "Content-Type: application/json" \
    --data-binary @- >/dev/null 2>&1 || true
curl -s --max-time 1.0 \
    "$SERVER/handoff?agent=claude-code" 2>/dev/null || true
exit 0
