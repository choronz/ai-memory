# ai-memory

> Long-term memory for AI coding agents. Quit Claude Code mid-task,
> start OpenAI Codex in the same directory, continue without
> re-explaining the architecture, the failed approaches, or the open
> questions.

[![status: v0.2 milestones complete](https://img.shields.io/badge/status-v0.2--complete-green)](docs/ARCHITECTURE.md)
[![Rust](https://img.shields.io/badge/rust-1.95+-blue)](rust-toolchain.toml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

## What it is

LLM coding agents lose all context when a session ends. ai-memory
gives them a shared, persistent wiki: every prompt, tool call, and
decision is captured automatically; when a session ends, the relevant
pages get rewritten as a coherent narrative; when the next agent
starts (Claude Code, Codex, OpenCode, …) it sees a handoff with
"where you left off" already prepended.

The wiki is plain markdown in a git repo — `grep`-able, openable in
Obsidian, backed up with `rsync`. No vector database to babysit, no
`write_note` ceremony, no manual context-loading. The full design is
in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md); the influences and
priors are at the [bottom](#influences-and-prior-art).

## Quick start

You need: Docker + an agent CLI (Claude Code, Codex, OpenCode, Cursor,
or anything else that speaks MCP).

```bash
# 1. Generate a bearer token (one-time; save the output).
export TOKEN=$(docker run --rm akitaonrails/ai-memory:latest generate-auth-token)

# 2. Start the server. Replace the API keys (or omit the four
#    -e AI_MEMORY_LLM_* / EMBEDDING_* lines for zero-LLM mode — FTS5
#    search still works without any keys).
docker run -d --name ai-memory \
    -p 49374:49374 \
    -v ai-memory-data:/data \
    -e AI_MEMORY_AUTH_TOKEN="$TOKEN" \
    -e AI_MEMORY_LLM_PROVIDER=anthropic \
    -e ANTHROPIC_API_KEY=sk-ant-... \
    -e AI_MEMORY_EMBEDDING_PROVIDER=openai \
    -e OPENAI_API_KEY=sk-... \
    akitaonrails/ai-memory:latest

# 3. Wire your agent CLI to it. Claude Code shown below; for Codex,
#    OpenCode, Cursor, Claude Desktop, Gemini CLI, OpenClaw, see
#    docs/install.md.

# 3a. Register the MCP endpoint
claude mcp add --transport http ai-memory http://localhost:49374/mcp \
    --header "Authorization: Bearer $TOKEN"

# 3b. Extract the bundled hook scripts to your home dir
docker cp ai-memory:/usr/local/share/ai-memory/hooks ~/.ai-memory/

# 3c. Render the settings.json snippet, then merge the printed JSON
#     into ~/.claude/settings.json (chmod 600 the file)
docker run --rm akitaonrails/ai-memory:latest \
    install-hooks --agent claude-code \
        --hooks-dir ~/.ai-memory/hooks \
        --server-url "http://localhost:49374" \
        --auth-token "$TOKEN"
```

That's it. Start a Claude Code session as usual — every prompt and
tool call now lands in ai-memory, and the next session you open in
this project will see a handoff with where you left off.

**For everything else** — Codex, OpenCode, Cursor, Claude Desktop,
Gemini CLI, OpenClaw, the curl-based hook installer (no docker
needed), running ai-memory without docker, the full subcommand
reference, the homelab deploy pattern, security hardening — see
[**`docs/install.md`**](docs/install.md).

## How it works in practice

You mostly don't think about it. Hooks capture every prompt + tool
call + session boundary automatically. The agent gains awareness of
prior work without you typing anything special. A few patterns are
worth knowing:

### Cross-agent handoff

```
$ claude
> "Working on the auth refactor. JWT rotation story is broken; trying
   session cookies as an alternative."
[work for an hour]
> /exit

$ codex   # in the same directory, hours or days later
[SessionStart hook fetches the handoff; the next agent sees it.]
> "Picking up: you were investigating session cookies as an
   alternative to broken JWT rotation. Continuing?"
```

You did nothing special. Handoff created automatically on Claude
Code's session-end, surfaced automatically on Codex's session-start.

### Compaction recovery

When Claude Code or Codex compact their working context, the
`PreCompact` hook fires and ai-memory writes a fresh
`sessions/<id>.md` page summarising the session so far. After
compaction, the agent can recover the summary via `memory_recent`
even though its raw history is gone.

### Spelunking your own history

```bash
docker exec ai-memory ls /data/wiki/sessions/
docker exec ai-memory cat /data/wiki/sessions/<uuid>.md

# Open in Obsidian / any markdown viewer:
docker cp ai-memory:/data/wiki ./my-ai-memory-wiki

# Time-travel:
docker exec ai-memory git -C /data/wiki log --oneline
```

### Nudge the agent to *use* memory proactively

Lifecycle hooks handle *capture* and *handoff resume* without you
typing anything. Proactive *querying* still depends on the agent
thinking to call `memory_query`. The tool descriptions nudge in that
direction; for projects where memory matters, drop this in your
`CLAUDE.md` / agent system prompt:

```markdown
## Long-term memory

This project uses ai-memory. The SessionStart hook auto-fetches any
pending handoff. Beyond that, proactively use:

- `memory_query` — before proposing architecture, when the user
  references prior work you don't recognise, or when investigating a
  bug that might have a known root cause.
- `memory_recent` — at session start to scan the last few pages.
- `memory_handoff_begin` — optional; only if you want extra context
  beyond what the SessionEnd hook captures by default.
```

## LLM provider — recommended defaults

You can run ai-memory entirely without an LLM (FTS5 search +
rule-based summaries, $0). When you do configure one, sensible
defaults kick in for the model:

| Provider env | Default model | Notes |
|---|---|---|
| `AI_MEMORY_LLM_PROVIDER=anthropic` | `claude-sonnet-4-6` | Smart enough to summarise a session, cheap enough to run on every session-end. |
| `AI_MEMORY_LLM_PROVIDER=openai` | `gpt-4o-mini` | OpenAI equivalent of the Sonnet tier in price/quality. |
| `AI_MEMORY_LLM_PROVIDER=openai-compat` | (required) | Set `AI_MEMORY_LLM_BASE_URL` + `AI_MEMORY_LLM_MODEL`. Use for Ollama, OpenRouter, vLLM, LM Studio. |

Embedding defaults (when `AI_MEMORY_EMBEDDING_PROVIDER` is set):

| Provider | Default model | Dim |
|---|---|---|
| `openai` | `text-embedding-3-small` | 1536 |
| `voyage` | `voyage-3` | 1024 |

Per-tier feature breakdown + the openai-compat / Ollama setup is in
[`docs/install.md`](docs/install.md#llm-provider-tiers).

## Architecture in 60 seconds

A single Rust binary, optionally containerised. Runs as an
[MCP](https://modelcontextprotocol.io/) server over stdio + HTTP.
Owns a data directory containing:

```
<data_dir>/
├── wiki/    # markdown source of truth (git-versioned)
├── raw/     # immutable session log archive
├── db/      # SQLite (FTS5 + page_embeddings) — derived index
├── models/  # reserved for local embedding model (v0.3+)
└── logs/    # rolling daily tracing output
```

Agent lifecycle hooks fire-and-forget POST to the server's HTTP
ingress. The server queues writes through a single SQLite writer
(no `database is locked`). On session end an optional LLM-driven pass
rewrites pages atomically with supersession (`is_latest=false` +
`supersedes` chain) and opens a typed handoff for the next agent.
The retention sweep decays unused episodic content while semantic
concept pages compound forever; pinned pages are exempt. Retrieval
is FTS5 by default; when an embedder is configured, hybrid RRF over
`page_embeddings` joins the FTS5 ranks.

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the canonical
data-flow diagram + crate breakdown + cross-cutting invariants.

## Docs

| File | What it is |
|---|---|
| [`docs/install.md`](docs/install.md) | **Installation cookbook.** Every agent CLI, every alternative (curl, source build, no-docker, no-auth). Read after the Quick start if your setup doesn't match the happy path. |
| [`docs/mcp-install.md`](docs/mcp-install.md) | Per-client MCP config snippets (Cursor, Claude Desktop, Gemini CLI, OpenClaw, pi). |
| [`docs/deploy.md`](docs/deploy.md) | Homelab deploy: bin/deploy, bearer-token auth, TLS via cloudflared. |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | Operational summary: data flow, crate layout, cross-cutting invariants, schema. |
| [`docs/design-decisions.md`](docs/design-decisions.md) | The full v1 spec. |
| Research docs under `docs/` | Karpathy LLM Wiki notes, agentmemory / basic-memory / cognee deep-dives, lessons-learned from upstream issues. |

## Influences and prior art

- **[Karpathy LLM Wiki](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f)** — the compile-not-retrieve pattern.
- **[agentmemory](https://github.com/rohitg00/agentmemory)** — most of the right ideas; this project is the Rust successor.
- **[basic-memory](https://github.com/basicmachines-co/basic-memory)** — the markdown-on-disk source-of-truth model.
- **[cognee](https://github.com/topoteretes/cognee)** — pipeline composition and triplet embeddings.
- **[A-MEM](https://arxiv.org/abs/2502.12110)** — Zettelkasten-style atomic notes with link evolution.

## License

Dual-licensed under MIT OR Apache-2.0.

## Acknowledgements

This codebase is being built collaboratively with Claude Code
(Anthropic Claude Opus 4.7) following the plan documented in
`docs/design-decisions.md`.
