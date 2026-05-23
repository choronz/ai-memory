#!/bin/sh
# cursor pre-tool-use hook.
# Forwards the event JSON to the ai-memory server. Adds an
# Authorization: Bearer header when AI_MEMORY_AUTH_TOKEN is set in
# this hook's environment (set it via install-hooks --auth-token).
SERVER="${AI_MEMORY_HOOK_URL:-http://127.0.0.1:49374}"
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
post_hook "$SERVER/hook?event=pre-tool-use&agent=cursor" >/dev/null 2>&1 || true
exit 0
