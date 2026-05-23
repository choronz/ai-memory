#!/bin/sh
# cursor SessionStart hook.
#
# 1. Forwards the event JSON to the ai-memory server (fire-and-forget).
# 2. Synchronously fetches any pending cross-agent handoff and prints
#    it to stdout — agent CLIs prepend session-start hook stdout to
#    the next session, so the resuming agent sees prior context with
#    no human in the loop.
#
# Both calls carry an Authorization: Bearer header when
# AI_MEMORY_AUTH_TOKEN is set in this hook's environment.
SERVER="${AI_MEMORY_HOOK_URL:-http://127.0.0.1:49374}"
PAYLOAD=$(cat)

post_hook() {
    if [ -n "${AI_MEMORY_AUTH_TOKEN:-}" ]; then
        curl -s --max-time 0.5 -X POST "$1" \
            -H "Content-Type: application/json" \
            -H "Authorization: Bearer $AI_MEMORY_AUTH_TOKEN" \
            --data-binary @-
    else
        curl -s --max-time 0.5 -X POST "$1" \
            -H "Content-Type: application/json" \
            --data-binary @-
    fi
}

get_handoff() {
    if [ -n "${AI_MEMORY_AUTH_TOKEN:-}" ]; then
        curl -s --max-time 1.0 "$1" \
            -H "Authorization: Bearer $AI_MEMORY_AUTH_TOKEN"
    else
        curl -s --max-time 1.0 "$1"
    fi
}

echo "$PAYLOAD" | post_hook "$SERVER/hook?event=session-start&agent=cursor" >/dev/null 2>&1 || true
get_handoff "$SERVER/handoff?agent=cursor" 2>/dev/null || true
exit 0
