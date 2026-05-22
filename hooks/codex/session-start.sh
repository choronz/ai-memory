#!/bin/sh
# codex SessionStart hook.
#
# Same shape as the claude-code variant: POST the event to /hook, then
# synchronously GET /handoff and echo the markdown so codex sees any
# pending cross-agent handoff at session start.
SERVER="${AI_MEMORY_HOOK_URL:-http://127.0.0.1:49374}"
PAYLOAD=$(cat)
echo "$PAYLOAD" | curl -s --max-time 0.5 \
    -X POST "$SERVER/hook?event=session-start&agent=codex" \
    -H "Content-Type: application/json" \
    --data-binary @- >/dev/null 2>&1 || true
curl -s --max-time 1.0 \
    "$SERVER/handoff?agent=codex" 2>/dev/null || true
exit 0
