# macOS Support

macOS is a supported platform: the workspace test suite runs on macOS CI and
tagged releases publish native `ai-memory-macos-aarch64.tar.gz` (Apple Silicon)
and `ai-memory-macos-x86_64.tar.gz` (Intel) binaries.

On macOS the **native binary** (a prebuilt release or a source build) is the
recommended way to run ai-memory today. It binds the server on
`127.0.0.1:49374`, and both the MCP endpoint and the lifecycle hooks talk to
that loopback address — which the native agent can reach and which is already in
the default Host-header allowlist. The Docker wrapper is the recommended path on
Linux, but on macOS it currently has the rough edges listed under
[Known limitations](#known-limitations-on-macos); prefer the native binary until
those are resolved.

Unlike Windows there is only one "path world" on macOS: POSIX paths and POSIX
`.sh` hooks throughout. There is no WSL-vs-native split to get wrong.

## Rule Of Thumb

Run `setup-agent` / `install-mcp` / `install-hooks` from the same shell that
launches Claude Code, Codex, Cursor, Gemini CLI, or another agent — on macOS
that is just your normal Terminal.

- The agent runs as a native macOS process, so its config must point at a
  **host-reachable** server URL. The native binary renders
  `http://127.0.0.1:49374`, which works. (The Docker wrapper renders
  `http://host.docker.internal:49374`, which does **not** resolve on the macOS
  host — see [Known limitations](#known-limitations-on-macos).)
- Hooks are rendered for one of two platforms:
  - `posix-native` — a direct `ai-memory hook --event …` call. The default for
    native macOS/Linux Claude Code installs (cargo / release binary); it uses
    the local event spool + OIDC-token fallback.
  - `posix` — `sh` runs the bundled `.sh` script. The Docker wrapper's default
    (the host has no local binary).

  Set `AI_MEMORY_HOOK_PLATFORM` before wiring hooks to override the default.

## Scenario A: Prebuilt Release Binary (Recommended, No Toolchain)

Use this when you want a local server plus native hooks without a Rust toolchain
or Docker. Each tagged release publishes a macOS tarball per architecture.

```bash
# 1. Download the archive for your chip and extract it to a stable location.
#    aarch64 = Apple Silicon (M-series); x86_64 = Intel.
mkdir -p ~/Applications/ai-memory && cd ~/Applications/ai-memory
curl -fsSL -O https://github.com/akitaonrails/ai-memory/releases/latest/download/ai-memory-macos-aarch64.tar.gz
tar -xzf ai-memory-macos-aarch64.tar.gz
# `curl` downloads are not Gatekeeper-quarantined, so the binary runs as-is.
# If you downloaded via a browser instead, clear the quarantine flag once:
#   xattr -d com.apple.quarantine ./ai-memory

# 2. Initialise the data dir (defaults to
#    ~/Library/Application Support/ai-memory; override with AI_MEMORY_DATA_DIR).
./ai-memory init

# 3. Start the server (loopback only).
./ai-memory serve --transport http --bind 127.0.0.1:49374
```

In a second terminal, stage the hook bundle and wire the agent:

```bash
cd ~/Applications/ai-memory
# `setup-agent` extracts the bundled hook scripts and writes the hook config.
# Pass --source ./hooks because the scripts ship beside the binary in the
# release tarball (see Known limitations #4).
./ai-memory setup-agent --agent claude-code --source ./hooks \
    --to ~/.local/share/ai-memory/hooks --apply
./ai-memory install-mcp --client claude-code --apply
```

Notes:

- The MCP endpoint and the capture hooks work without a token in this
  single-user loopback setup. Admin/diagnostic routes are gated, so
  `ai-memory status` needs the bearer token `init` configures for new installs —
  pass `--auth-token <token>` or export `AI_MEMORY_AUTH_TOKEN`.
- Keep the extracted `ai-memory` at a stable path; the hook commands reference
  it. Re-run `setup-agent` / `install-hooks` if you move it.

## Scenario B: Source Build

Use this when developing ai-memory itself. Requires Rust 1.95
(`rust-toolchain.toml`) plus the Xcode Command Line Tools
(`xcode-select --install`); SQLite is bundled and libgit2 is vendored, so no
extra system libraries are needed.

```bash
git clone https://github.com/akitaonrails/ai-memory
cd ai-memory
cargo build --release --workspace
./target/release/ai-memory init
./target/release/ai-memory serve --transport http --bind 127.0.0.1:49374
```

From another shell in the repo, `install-hooks` finds the bundled `hooks/`
automatically (no `--source` needed from the repo root):

```bash
./target/release/ai-memory install-hooks --agent claude-code --apply
./target/release/ai-memory install-mcp   --client claude-code --apply
```

## Scenario C: Docker Wrapper

The server itself runs fine in Docker on macOS; the rough edges are in how the
macOS wrapper wires the **native** agent and how its thin-client CLI reaches the
server (see [Known limitations](#known-limitations-on-macos)). If you still want
the Docker server on macOS, apply both workarounds:

```bash
# Start the server, allowlisting the Docker-Desktop host alias so the wrapper's
# own thin-client commands (status, search, …) are not rejected with 403.
docker run -d --name ai-memory --restart unless-stopped \
    -p 127.0.0.1:49374:49374 -v ai-memory-data:/data \
    -e AI_MEMORY_ALLOWED_HOSTS="localhost,127.0.0.1,::1,host.docker.internal" \
    akitaonrails/ai-memory:latest

# Wire the agent with an explicit loopback URL so install-mcp/install-hooks emit
# a host-reachable address instead of the unresolvable host.docker.internal.
AI_MEMORY_SERVER_URL=http://127.0.0.1:49374 \
    ai-memory install-mcp   --client claude-code --apply
AI_MEMORY_SERVER_URL=http://127.0.0.1:49374 \
    ai-memory install-hooks --agent  claude-code --apply
```

On Apple Silicon the published `:latest` image currently runs as `linux/amd64`
under emulation (Docker prints a platform-mismatch warning); it works but is not
native.

## Hook Platform on macOS

`AI_MEMORY_HOOK_PLATFORM` selects how hook commands are rendered. On macOS the
two relevant values are `posix-native` (direct binary call; the native default)
and `posix` (the bundled `.sh` scripts; the Docker-wrapper default). Set it
before running `setup-agent` / `install-hooks` so the choice is baked into the
rendered commands. The native hook spools events locally and drains them at
session boundaries; the whole-minute spool-timing overrides are shared with
Windows and documented in
[`docs/windows.md`](windows.md#tuning-the-spool-timings-high-latency-instances).

## Known limitations on macOS

These were observed on Apple Silicon (macOS 26, Docker Desktop 28). Until they
are fixed, prefer the native binary (Scenario A/B).

1. **`:latest` runs under emulation on Apple Silicon.** The published `:latest`
   tag currently resolves to a single `linux/amd64` image, so Docker Desktop
   runs it through emulation with a platform-mismatch warning rather than
   natively on `arm64`.
2. **Docker wrapper thin-client → `403 forbidden host`.** The macOS wrapper
   reaches the server via `host.docker.internal`, which is not in the default
   Host-header allowlist (`localhost`, `127.0.0.1`, `::1`). Add
   `host.docker.internal` to `AI_MEMORY_ALLOWED_HOSTS` on the server (Scenario C).
3. **Docker wrapper renders an unreachable agent URL.** Run through the wrapper,
   `install-mcp` / `install-hooks` emit `http://host.docker.internal:49374`,
   which does not resolve on the macOS host — so the native agent's MCP client
   and capture hooks cannot reach the server. Wire with
   `AI_MEMORY_SERVER_URL=http://127.0.0.1:49374` (Scenario C).
4. **Release-binary hook discovery needs `--source`.** `setup-agent` /
   `install-hooks` do not auto-locate the `hooks/` bundle that ships beside the
   binary in the release tarball, so pass `--source ./hooks` (Scenario A).

## Suggested Test Checklist

1. `ai-memory serve --bind 127.0.0.1:49374` starts and logs `bind=127.0.0.1:49374`.
2. `curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:49374/mcp` returns
   `405` (reachable; GET not allowed), confirming the loopback server is up.
3. `setup-agent --agent claude-code --source ./hooks --to … --apply` extracts the
   hook scripts and writes a config whose commands reference
   `http://127.0.0.1:49374` and host-side `.sh` paths.
4. `install-mcp --client claude-code` renders `http://127.0.0.1:49374/mcp`.
5. Launch the agent, call `memory_status`, send a prompt, then confirm capture
   (`ai-memory status --auth-token <token>` shows non-zero observations, or query
   the SQLite `observations` table).

Report which scenario you used, your chip (Apple Silicon / Intel), the agent and
version, and whether hooks executed or failed with a connect/resolve error.
