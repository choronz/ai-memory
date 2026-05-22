# ai-memory

> Long-term memory for AI coding agents. Quit Claude Code mid-task, start
> OpenAI Codex in the same directory, continue without re-explaining the
> architecture, the failed approaches, or the open questions.

[![status: v0.2 milestones complete](https://img.shields.io/badge/status-v0.2--complete-green)](docs/ARCHITECTURE.md)
[![Rust](https://img.shields.io/badge/rust-1.95+-blue)](rust-toolchain.toml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

## Why this exists

LLM coding agents lose all context when a session ends. Today's
"memory" tools either (a) require the user to manually invoke
`write_note` every time something matters, or (b) wrap a vector
database in a chat shim and call it RAG.

ai-memory takes a different bet, faithful to
[Andrej Karpathy's "LLM Wiki"](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f)
pattern: knowledge is **compiled** at ingest time into a structured,
cross-linked, supersedeable wiki on disk — not retrieved over raw logs
at query time. The wiki is plain markdown in a git repo, so you can
`grep` it, open it in Obsidian, diff it, and back it up with `rsync`.

Capture is **fully automatic** via the agent CLI's lifecycle hooks; there
is no `write_note` ceremony. Consolidation runs in the background when
a session ends and when the agent's own context window is about to be
compacted — both moments where state would otherwise be lost.

## Status

**v0.2 complete.** Milestones M0 through M10 have shipped. Single
package; 57+ unit + integration tests passing, `cargo clippy
--workspace -D warnings` clean, `cargo fmt --check` clean.

## Quick start (Docker)

The recommended way to run ai-memory is as a long-lived container on
your dev machine or homelab. The image is published to Docker Hub at
[`akitaonrails/ai-memory:latest`](https://hub.docker.com/r/akitaonrails/ai-memory).

```bash
# Zero-LLM mode: pure FTS5 search, rule-based session summaries.
# No API keys needed. Good for trying it out.
docker run -d --name ai-memory \
    -p 49374:49374 \
    -v ai-memory-data:/data \
    akitaonrails/ai-memory:latest
```

```bash
# Full mode: LLM-driven consolidation + hybrid search.
# Recommended defaults: Claude Sonnet for consolidation,
# OpenAI text-embedding-3-small for embeddings.
docker run -d --name ai-memory \
    -p 49374:49374 \
    -v ai-memory-data:/data \
    -e AI_MEMORY_LLM_PROVIDER=anthropic \
    -e ANTHROPIC_API_KEY=sk-ant-... \
    -e AI_MEMORY_EMBEDDING_PROVIDER=openai \
    -e OPENAI_API_KEY=sk-... \
    akitaonrails/ai-memory:latest
```

The server listens on `127.0.0.1:49374` (port `0xC0DE` — "CODE" in hex,
in IANA's dynamic/private range so it won't clash with registered
services). MCP endpoint: `http://127.0.0.1:49374/mcp`. Hook ingress:
`http://127.0.0.1:49374/hook`.

Inspect state with `docker exec ai-memory ai-memory status --json`.
Wiki and SQLite live in the `ai-memory-data` volume; back up with
`docker run --rm -v ai-memory-data:/data busybox tar -czf - /data >
ai-memory-$(date +%F).tar.gz`.

## Configure your agent CLI

Each agent CLI needs two things wired up:

1. **MCP connection** — so the agent can call `memory_query`,
   `memory_recent`, etc.
2. **Lifecycle hooks** — so the server auto-captures session events.
   This is what makes capture "free".

### Claude Code

```bash
# 1. Register the MCP server (HTTP transport).
claude mcp add --transport http ai-memory http://127.0.0.1:49374/mcp

# 2. Install the lifecycle hooks. Clone the repo to get the scripts:
git clone https://github.com/akitaonrails/ai-memory ~/.ai-memory
cd ~/.ai-memory && cargo build --release
# Then print the JSON to merge into ~/.claude/settings.json:
~/.ai-memory/target/release/ai-memory install-hooks --agent claude-code
```

The hook scripts are tiny POSIX shell wrappers that `curl` the event
JSON to `127.0.0.1:49374/hook` with sub-second timeouts. They never
block the agent.

### OpenAI Codex

```bash
# 1. MCP — add to ~/.codex/config.toml:
[mcp_servers.ai-memory]
url = "http://127.0.0.1:49374/mcp"

# 2. Lifecycle hooks — Codex uses its own hooks file; the install
# helper prints the snippet to merge:
~/.ai-memory/target/release/ai-memory install-hooks --agent codex
```

### OpenCode

```bash
# Same pattern; see `ai-memory install-hooks --agent opencode` for
# the agent-specific config format.
~/.ai-memory/target/release/ai-memory install-hooks --agent opencode
```

## LLM provider, models, and API keys

ai-memory works in three intensity tiers. **The default Docker run is
the zero-LLM tier** — no API keys required, everything still works,
just with deterministic rule-based summaries and FTS5-only retrieval.

| Tier | What you get | Env vars | Cost |
|---|---|---|---|
| **Zero-LLM** | FTS5 search, rule-based session pages, auto-handoffs | (none) | $0 |
| **+ LLM consolidation** | LLM rewrites session pages as coherent narratives; LLM-driven contradiction lint; PreCompact checkpoints | `AI_MEMORY_LLM_PROVIDER=anthropic` + `ANTHROPIC_API_KEY` | ~$0.01–0.05 per session |
| **+ Hybrid retrieval** | RRF over FTS5 + vector cosine similarity. Better recall on paraphrased queries | `AI_MEMORY_EMBEDDING_PROVIDER=openai` + `OPENAI_API_KEY` | ~$0.0001 per page on backfill |

### Recommended models (chosen as defaults)

If you set the **provider** but not the model, ai-memory picks
sensible defaults. Override with the corresponding env var.

| Setting | Default | Why this default | Override |
|---|---|---|---|
| Anthropic LLM | `claude-sonnet-4-7` | Smart enough to summarise a session into coherent narrative; cheap enough to run on every session-end without thinking about it. Not a reasoning model — consolidation doesn't benefit from extended thinking. | `AI_MEMORY_LLM_MODEL` |
| OpenAI LLM | `gpt-4o-mini` | Closest OpenAI equivalent to the Sonnet tier in price/quality. | `AI_MEMORY_LLM_MODEL` |
| OpenAI embedding | `text-embedding-3-small` (1536-dim) | Best price/quality from OpenAI; 5× cheaper than `text-embedding-3-large` with marginal recall loss. | `AI_MEMORY_EMBEDDING_MODEL` + `AI_MEMORY_EMBEDDING_DIM` |
| Voyage embedding | `voyage-3` (1024-dim) | Voyage's current general-purpose recommendation. | same |

For **self-hosted LLMs** (Ollama, vLLM, LM Studio):

```bash
docker run -d --name ai-memory \
    -p 49374:49374 \
    -v ai-memory-data:/data \
    -e AI_MEMORY_LLM_PROVIDER=openai-compat \
    -e AI_MEMORY_LLM_BASE_URL=http://host.docker.internal:11434/v1 \
    -e AI_MEMORY_LLM_MODEL=qwen2.5-coder:14b \
    akitaonrails/ai-memory:latest
```

There is no safe default model for `openai-compat` — what you can run
depends on your local setup, so the model env var is required.

## How to use it in practice

The point of ai-memory is that you mostly **don't** think about it.
Lifecycle hooks capture every prompt + tool call + session boundary
automatically. The agent gains awareness of prior work without you
typing anything special. That said, a few patterns are worth knowing:

### Cross-agent handoff (the headline feature)

```
$ claude
> "Working on the auth refactor. The current approach uses JWT but
   we found the rotation story is broken. Investigating session
   cookies as an alternative."
[work for an hour]
> /exit

$ codex   # in the same directory, hours or days later
[Codex starts; the SessionStart hook synchronously fetches the open
 handoff and prepends it to the session.]
> "Picking up from prior work: you were investigating session cookies
   as an alternative to broken JWT rotation. Continuing?"
```

You did nothing special. The handoff was created automatically on
Claude Code's session-end, and surfaced automatically on Codex's
session-start.

### Context-window compaction recovery

When Claude Code or Codex compact their working context, the
`PreCompact` hook fires and ai-memory writes a fresh
`sessions/<id>.md` page summarising the session so far (LLM-driven
if you've configured a provider, rule-based otherwise). After
compaction, the agent can call `memory_recent` to recover the
high-signal summary even though its raw history is gone.

### Spelunking your own history

The wiki dir is plain markdown in a git repo:

```bash
docker exec ai-memory ls /data/wiki/sessions/
docker exec ai-memory cat /data/wiki/sessions/<uuid>.md

# Open in Obsidian or any markdown viewer:
docker cp ai-memory:/data/wiki ./my-ai-memory-wiki
obsidian ./my-ai-memory-wiki

# Time-travel:
docker exec ai-memory git -C /data/wiki log --oneline
```

### Nudging the agent to use memory proactively

Lifecycle hooks handle *capture* and *handoff resume* without you
typing anything. Proactive *querying* (e.g. "did we already decide
on this?") still depends on the agent thinking to call
`memory_query`. The tool descriptions already nudge in that
direction, but for projects where memory is load-bearing, drop this
snippet into your project's `CLAUDE.md`:

```markdown
## Long-term memory

This project uses ai-memory. The SessionStart hook auto-fetches any
pending handoff. Beyond that, proactively use:

- `memory_query` — before proposing architecture, when the user
  references prior work you don't recognise, or when investigating
  a bug that might have a known root cause.
- `memory_recent` — at session start (in addition to the auto-fetched
  handoff) to scan the last few pages.
- `memory_handoff_begin` — optional; only if you want to capture
  extra context beyond what the SessionEnd hook captures by default.
```

This is the least obtrusive way to encourage proactive use — it
lives in the project's own instructions rather than every user
prompt.

## CLI reference (when not using Docker)

If you'd rather run ai-memory directly:

```bash
cargo build --release --workspace
./target/release/ai-memory init                       # create data dir
./target/release/ai-memory serve --transport http \
    --bind 127.0.0.1:49374                            # MCP HTTP + hooks
./target/release/ai-memory status --json              # counts + paths
./target/release/ai-memory search "karpathy"          # FTS5 query
./target/release/ai-memory backup --to bak.tar.gz     # snapshot
./target/release/ai-memory restore --from bak.tar.gz  # restore
./target/release/ai-memory forget-sweep [--dry-run]   # retention
./target/release/ai-memory lint        [--dry-run]    # contradiction + orphan audit
./target/release/ai-memory embed       [--dry-run]    # backfill embeddings
./target/release/ai-memory --help                     # full subcommand tree
```

Data dir defaults to `~/.local/share/ai-memory` on Linux,
`~/Library/Application Support/ai-memory` on macOS. Override with
`AI_MEMORY_DATA_DIR=/path`.

## Architecture in 60 seconds

A single Rust binary, optionally containerised. Runs as an
[MCP](https://modelcontextprotocol.io/) server over stdio + HTTP. Owns
a data directory containing:

```
<data_dir>/
├── wiki/        # markdown source of truth (git-versioned)
├── raw/         # immutable session log archive
├── db/          # SQLite (FTS5 + page_embeddings) — derived index
├── models/      # reserved for local embedding model (v0.3+)
└── logs/        # rolling daily tracing output
```

Agent lifecycle hooks fire-and-forget POST to the server's HTTP
ingress. The server queues writes through a single SQLite writer
(no `database is locked`). On session end an optional LLM-driven pass
rewrites pages atomically with supersession (`is_latest=false` +
`supersedes` chain) and opens a typed handoff for the next agent. The
retention sweep decays unused episodic content while semantic concept
pages compound forever; pinned pages are exempt. Retrieval is FTS5
by default; when an embedder is configured, hybrid RRF over
`page_embeddings` joins the FTS5 ranks.

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the canonical
data-flow diagram + crate breakdown + cross-cutting invariants.

## Docs

| File | What it is |
|---|---|
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | **Read first.** Operational summary: data flow, crate layout, cross-cutting invariants, current schema. |
| [`docs/design-decisions.md`](docs/design-decisions.md) | The full v1 spec — storage, MCP surface, hooks, lifecycle, mistakes-to-avoid checklist. |
| [`research-karpathy-llm-wiki.md`](docs/research-karpathy-llm-wiki.md) | What Karpathy actually said + community extensions, with sources. |
| [`research-agentmemory.md`](docs/research-agentmemory.md) | Deep-dive on the TypeScript predecessor; ideas to reuse and substrate to drop. |
| [`research-basic-memory.md`](docs/research-basic-memory.md) | The manual-write-note model we explicitly diverge from. |
| [`research-cognee.md`](docs/research-cognee.md) | Knowledge-graph pipeline ideas to adopt + dependency landmines to avoid. |
| [`issues-agentmemory.md`](docs/issues-agentmemory.md) | Operational landmines from the upstream tracker. |
| [`issues-basic-memory.md`](docs/issues-basic-memory.md) | File-watcher + capture-friction landmines. |
| [`issues-cognee.md`](docs/issues-cognee.md) | LLM-gateway + multi-store landmines. |

[`CLAUDE.md`](CLAUDE.md) at the repo root holds the per-session
operating rules for the maintainer's own Claude Code sessions.

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
