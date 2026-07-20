#!/bin/sh
# kimi-code SessionStart hook.
# 1. Forwards the event JSON to the ai-memory server (fire-and-forget).
# 2. Synchronously fetches any pending cross-agent handoff and prints
#    it to stdout — kimi-code appends exit-0 hook stdout to the agent's
#    context, so the resuming agent sees prior context with no human
#    in the loop.
#
_lib_dir="$(dirname "$0")"
[ -f "$_lib_dir/_lib.sh" ] || _lib_dir="$_lib_dir/.."
. "$_lib_dir/_lib.sh"

SERVER="${AI_MEMORY_HOOK_URL:-http://127.0.0.1:49374}"
PAYLOAD=$(cat)
CWD=$(ai_memory_extract_cwd "$PAYLOAD")
QS=$(ai_memory_marker_qs "$CWD")

printf '%s' "$PAYLOAD" \
    | ai_memory_post_hook "$SERVER/hook?event=session-start&agent=kimi-code${QS}" >/dev/null 2>&1 || true
ai_memory_get_handoff "$SERVER/handoff?agent=kimi-code${QS}" 2>/dev/null || true
exit 0
