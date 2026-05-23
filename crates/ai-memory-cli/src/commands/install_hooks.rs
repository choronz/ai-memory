//! `ai-memory install-hooks` — print the suggested lifecycle-hook
//! configuration for the chosen agent CLI.
//!
//! In M3 this is *non-destructive*: we render the JSON snippet the user
//! should merge into their agent CLI's settings file, plus the absolute
//! paths to the vendored shell scripts. We intentionally do not mutate
//! `~/.claude/settings.json` automatically — agent CLI hook formats are
//! still in flux and bad merges are very user-visible.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::cli::{AgentChoice, InstallHooksArgs};
use crate::commands::apply_shared::{ApplyOutcome, apply_atomic, mutate_json};
use crate::commands::render_shared::{
    CURSOR_PROFILE, GEMINI_PROFILE, build_claude_code_payload, build_codex_payload,
    build_profile_payload,
};
use crate::config::Config;

/// Run the `install-hooks` subcommand.
///
/// # Errors
/// Returns an error if the hook script directory cannot be located.
pub fn run(_config: &Config, args: InstallHooksArgs) -> Result<()> {
    let hooks_dir = resolve_hooks_dir(args.hooks_dir.as_deref(), args.agent)?;
    let auth = args.auth_token.as_deref();
    if args.apply {
        return match args.agent {
            AgentChoice::ClaudeCode => {
                apply_to_claude_code_settings(&hooks_dir, &args.server_url, auth, &args)
            }
            AgentChoice::Codex => {
                apply_to_codex_settings(&hooks_dir, &args.server_url, auth, &args)
            }
            AgentChoice::Cursor => {
                apply_to_cursor_settings(&hooks_dir, &args.server_url, auth, &args)
            }
            AgentChoice::GeminiCli => {
                apply_to_gemini_settings(&hooks_dir, &args.server_url, auth, &args)
            }
            AgentChoice::OpenCode => {
                apply_to_opencode_plugin(&hooks_dir, &args.server_url, auth, &args)
            }
            AgentChoice::Openclaw => {
                println!(
                    "OpenClaw does not support lifecycle hooks (only HTTP webhooks for \
                     request ingress; no session/tool/prompt callbacks). ai-memory's \
                     hook surface relies on per-event POSTs, which OpenClaw cannot fire."
                );
                println!();
                println!("Workarounds if you want some capture against OpenClaw:");
                println!("  - Manually call `memory_handoff_begin` from your OpenClaw");
                println!("    session before wrapping up (it's still in the MCP surface).");
                println!("  - Or run a sidecar that observes OpenClaw's webhooks and");
                println!("    forwards them to ai-memory.");
                Ok(())
            }
        };
    }
    match args.agent {
        AgentChoice::ClaudeCode => render_claude_code(&hooks_dir, &args.server_url, auth),
        AgentChoice::Codex => render_agent("codex", &hooks_dir, &args.server_url, auth),
        AgentChoice::Cursor => render_agent("cursor", &hooks_dir, &args.server_url, auth),
        AgentChoice::GeminiCli => render_agent("gemini-cli", &hooks_dir, &args.server_url, auth),
        AgentChoice::OpenCode => render_agent("opencode", &hooks_dir, &args.server_url, auth),
        AgentChoice::Openclaw => {
            println!("OpenClaw does not expose lifecycle hooks — only HTTP webhooks.");
            println!("ai-memory cannot wire automatic capture against OpenClaw today.");
            Ok(())
        }
    }
}

/// Mutate `~/.claude/settings.json` in place: replace the seven hook
/// entries ai-memory cares about; preserve every other hook the user
/// has wired up to other tools.
fn apply_to_claude_code_settings(
    hooks_dir: &Path,
    server_url: &str,
    auth_token: Option<&str>,
    args: &InstallHooksArgs,
) -> Result<()> {
    let path = match &args.config_file {
        Some(p) => p.clone(),
        None => dirs::home_dir()
            .context("could not locate $HOME for ~/.claude/settings.json")?
            .join(".claude")
            .join("settings.json"),
    };
    let payload = build_claude_code_payload(hooks_dir, server_url, auth_token);
    let our_hooks = payload
        .get("hooks")
        .and_then(|v| v.as_object())
        .context("internal: build_claude_code_payload didn't return a hooks object")?
        .clone();
    let outcome = apply_atomic(&path, |existing| {
        mutate_json(existing, |root| {
            // Get-or-create the top-level `hooks` table, then OVERLAY
            // our seven event keys onto the user's table. Anything
            // they had under a non-overlapping event name (e.g. a
            // hand-written "Notification" hook) survives.
            let hooks = root
                .entry("hooks")
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
                .as_object_mut()
                .context("`hooks` is present in settings.json but not an object")?;
            for (event, value) in &our_hooks {
                hooks.insert(event.clone(), value.clone());
            }
            Ok(())
        })
    })?;
    println!(
        "✓ {} {} ({})",
        outcome.verb(),
        path.display(),
        match outcome {
            ApplyOutcome::Created => "new file",
            ApplyOutcome::Updated => "backup written next to it",
            ApplyOutcome::NoOp => "already up to date",
        }
    );
    Ok(())
}

/// Mutate `~/.codex/hooks.json` (creating it if absent) so Codex's
/// lifecycle hook runner fires the ai-memory scripts on every
/// session/prompt/tool event.
///
/// Codex's hook config is structurally identical to Claude Code's
/// (verified against `openai/codex/codex-rs/config/src/hooks_tests.rs`):
///
///   { "hooks": {
///       "SessionStart": [
///         { "matcher": "",
///           "hooks": [ {"type":"command", "command":"..."} ]
///         }
///       ], ...
///   } }
///
/// Codex looks for hooks in `~/.codex/hooks.json` by default (or
/// wherever `hooks = "./relative-path.json"` in config.toml points).
/// We write the standalone file and don't touch config.toml — Codex
/// picks it up automatically.
///
/// Trust note: Codex refuses to RUN new hooks until the user accepts
/// them in the TUI ("Trust all and continue") or sets
/// `--dangerously-bypass-hook-trust`. We print a reminder.
fn apply_to_codex_settings(
    hooks_dir: &Path,
    server_url: &str,
    auth_token: Option<&str>,
    args: &InstallHooksArgs,
) -> Result<()> {
    let path = match &args.config_file {
        Some(p) => p.clone(),
        None => dirs::home_dir()
            .context("could not locate $HOME for ~/.codex/hooks.json")?
            .join(".codex")
            .join("hooks.json"),
    };
    // Build the Codex-flavoured payload. The JSON shape is identical
    // to Claude Code's matcher + nested hooks form — only the event
    // list differs (no `SessionEnd`, which Codex doesn't recognise).
    let payload = build_codex_payload(hooks_dir, server_url, auth_token);
    let our_hooks = payload
        .get("hooks")
        .and_then(|v| v.as_object())
        .context("internal: payload builder didn't return a hooks object")?
        .clone();
    let outcome = apply_atomic(&path, |existing| {
        mutate_json(existing, |root| {
            let hooks = root
                .entry("hooks")
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
                .as_object_mut()
                .context("`hooks` is present in hooks.json but not an object")?;
            // Remove any stale `SessionEnd` entry left behind by an
            // earlier version of install-hooks that mistakenly wrote
            // the Claude-Code-only event into Codex's file. Codex
            // ignores unknown events but the file looks cleaner
            // without dead keys.
            hooks.remove("SessionEnd");
            for (event, value) in &our_hooks {
                hooks.insert(event.clone(), value.clone());
            }
            Ok(())
        })
    })?;
    println!(
        "✓ {} {} ({})",
        outcome.verb(),
        path.display(),
        match outcome {
            ApplyOutcome::Created => "new file",
            ApplyOutcome::Updated => "backup written next to it",
            ApplyOutcome::NoOp => "already up to date",
        }
    );
    // First-time trust reminder. Codex's TUI flags new/changed
    // hooks on startup; users must explicitly trust them before
    // they fire.
    if !matches!(outcome, ApplyOutcome::NoOp) {
        println!();
        println!("Codex requires explicit trust for new hooks. Next time you start `codex`:");
        println!("  → the TUI will surface 'Hooks need review' for each new event");
        println!("  → choose 'Trust all and continue' (or trust individually)");
        println!("To bypass the prompt for automated installs, start with");
        println!("`codex --dangerously-bypass-hook-trust` (review hook scripts first).");
    }
    Ok(())
}

/// Mutate `~/.cursor/hooks.json` (creating it if absent) so Cursor's
/// agent fires the ai-memory scripts on lifecycle events.
///
/// Cursor's hook schema (per <https://cursor.com/docs/agent/hooks>) is
/// *flatter* than Claude Code's / Codex's:
///
///   { "version": 1,
///     "hooks": {
///       "sessionStart": [
///         { "type": "command", "command": "...", "matcher": "" }
///       ]
///     }
///   }
///
/// — no inner `hooks: [...]` array, camelCase event names, plus a
/// required top-level `version: 1` key. We use `CURSOR_PROFILE`
/// (HookShape::Flat) to produce the right payload, then merge into
/// the existing file (preserving any non-overlapping events the
/// user has wired up to other tools).
fn apply_to_cursor_settings(
    hooks_dir: &Path,
    server_url: &str,
    auth_token: Option<&str>,
    args: &InstallHooksArgs,
) -> Result<()> {
    let path = match &args.config_file {
        Some(p) => p.clone(),
        None => dirs::home_dir()
            .context("could not locate $HOME for ~/.cursor/hooks.json")?
            .join(".cursor")
            .join("hooks.json"),
    };
    let payload = build_profile_payload(&CURSOR_PROFILE, hooks_dir, server_url, auth_token);
    let our_hooks = payload
        .get("hooks")
        .and_then(|v| v.as_object())
        .context("internal: payload builder didn't return a hooks object")?
        .clone();
    let outcome = apply_atomic(&path, |existing| {
        mutate_json(existing, |root| {
            // Cursor requires "version": 1 at the top level.
            // Overwrite unconditionally — the schema is versioned
            // so future Cursor releases can bump this; we'll bump
            // here too when that happens.
            root.insert("version".into(), serde_json::json!(1));
            let hooks = root
                .entry("hooks")
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
                .as_object_mut()
                .context("`hooks` is present in hooks.json but not an object")?;
            for (event, value) in &our_hooks {
                hooks.insert(event.clone(), value.clone());
            }
            Ok(())
        })
    })?;
    println!(
        "✓ {} {} ({})",
        outcome.verb(),
        path.display(),
        match outcome {
            ApplyOutcome::Created => "new file",
            ApplyOutcome::Updated => "backup written next to it",
            ApplyOutcome::NoOp => "already up to date",
        }
    );
    Ok(())
}

/// Mutate `~/.gemini/settings.json` so Gemini CLI fires the ai-memory
/// scripts on its (Gemini-specific) lifecycle events.
///
/// Gemini's schema (per <https://geminicli.com/docs/hooks/reference>)
/// is the same nested shape as Claude Code's (`matcher` +
/// `hooks: [{type, command}]`), but the event vocabulary differs:
///
///   - `BeforeTool` / `AfterTool`  (ai-memory: `pre-tool-use` / `post-tool-use`)
///   - `PreCompress`               (ai-memory: `pre-compact`)
///   - `SessionStart` / `SessionEnd` line up with Claude Code's
///   - No `UserPromptSubmit` / `Stop` equivalents — skipped
///
/// Like Claude Code, Gemini doesn't honour an `env` field at the
/// inner-hook level, so the env vars get inlined into the command
/// string by the shared payload builder.
fn apply_to_gemini_settings(
    hooks_dir: &Path,
    server_url: &str,
    auth_token: Option<&str>,
    args: &InstallHooksArgs,
) -> Result<()> {
    let path = match &args.config_file {
        Some(p) => p.clone(),
        None => dirs::home_dir()
            .context("could not locate $HOME for ~/.gemini/settings.json")?
            .join(".gemini")
            .join("settings.json"),
    };
    let payload = build_profile_payload(&GEMINI_PROFILE, hooks_dir, server_url, auth_token);
    let our_hooks = payload
        .get("hooks")
        .and_then(|v| v.as_object())
        .context("internal: payload builder didn't return a hooks object")?
        .clone();
    let outcome = apply_atomic(&path, |existing| {
        mutate_json(existing, |root| {
            // Gemini's settings.json mixes MCP servers, hooks, and
            // other config under one document. Get-or-create the
            // `hooks` table; overlay our events; preserve siblings.
            let hooks = root
                .entry("hooks")
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
                .as_object_mut()
                .context("`hooks` is present in settings.json but not an object")?;
            for (event, value) in &our_hooks {
                hooks.insert(event.clone(), value.clone());
            }
            Ok(())
        })
    })?;
    println!(
        "✓ {} {} ({})",
        outcome.verb(),
        path.display(),
        match outcome {
            ApplyOutcome::Created => "new file",
            ApplyOutcome::Updated => "backup written next to it",
            ApplyOutcome::NoOp => "already up to date",
        }
    );
    Ok(())
}

/// Generate an OpenCode plugin at `~/.config/opencode/plugins/ai-memory.ts`.
///
/// Unlike Claude Code / Codex / Cursor / Gemini, OpenCode's lifecycle
/// hooks aren't JSON config — they're TypeScript modules under
/// `~/.config/opencode/plugins/` (per
/// <https://opencode.ai/docs/plugins/>). A plugin exports an
/// async function returning a hooks object; the keys are
/// dot-separated event names (`session.created`, `tool.execute.before`,
/// etc.) and the values are async handlers.
///
/// Event mapping (OpenCode → ai-memory):
///   session.created      → session-start.sh
///   session.idle         → stop.sh
///   session.compacted    → pre-compact.sh
///   tool.execute.before  → pre-tool-use.sh
///   tool.execute.after   → post-tool-use.sh
///
/// OpenCode doesn't expose a `prompt-submit` equivalent or a
/// `session.ended` event (idle is the closest). We forward what
/// we can.
fn apply_to_opencode_plugin(
    hooks_dir: &Path,
    server_url: &str,
    auth_token: Option<&str>,
    args: &InstallHooksArgs,
) -> Result<()> {
    let path = match &args.config_file {
        Some(p) => p.clone(),
        None => dirs::home_dir()
            .context("could not locate $HOME for ~/.config/opencode/plugins")?
            .join(".config")
            .join("opencode")
            .join("plugins")
            .join("ai-memory.ts"),
    };
    let script = |name: &str| {
        hooks_dir
            .join(name)
            .to_string_lossy()
            .into_owned()
    };
    let token_line = auth_token
        .map(|t| format!("const TOKEN = {};\n", ts_string_literal(t)))
        .unwrap_or_else(|| "const TOKEN = null;\n".to_string());
    let body = format!(
        r#"// Auto-generated by `ai-memory install-hooks --agent open-code --apply`.
// Edit by re-running the command, not by hand — install-hooks
// will overwrite this file (with a `.bak-<ts>` backup) on each
// re-run.

import type {{ Plugin }} from "@opencode-ai/plugin";

const SERVER = {server_literal};
{token_line}
async function fire(
  $: any,
  script: string,
  event: string,
  payload: unknown,
): Promise<void> {{
  // The hook scripts read JSON from stdin + AI_MEMORY_HOOK_URL/
  // AI_MEMORY_AUTH_TOKEN from env. Inline the env into the shell
  // command via POSIX `VAR=val cmd` syntax so we don't depend on
  // the runtime's env-passing semantics.
  const body = JSON.stringify({{
    hook_event_name: event,
    agent: "open-code",
    payload,
  }});
  try {{
    const authPrefix = TOKEN ? `AI_MEMORY_AUTH_TOKEN=${{TOKEN}} ` : "";
    await $`echo ${{body}} | env AI_MEMORY_HOOK_URL=${{SERVER}} ${{authPrefix}}${{script}}`
      .nothrow()
      .quiet();
  }} catch (_e) {{
    // Fire-and-forget. Hooks must never block the agent.
  }}
}}

export const AiMemoryHooks: Plugin = async ({{ $ }}) => {{
  return {{
    "session.created": async (input) => fire($, {session_start}, "session-start", input),
    "session.idle":    async (input) => fire($, {stop},          "stop", input),
    "session.compacted": async (input) => fire($, {pre_compact}, "pre-compact", input),
    "tool.execute.before": async (input) => fire($, {pre_tool},  "pre-tool-use", input),
    "tool.execute.after":  async (input) => fire($, {post_tool}, "post-tool-use", input),
  }};
}};
"#,
        server_literal = ts_string_literal(server_url),
        token_line = token_line,
        session_start = ts_string_literal(&script("session-start.sh")),
        stop = ts_string_literal(&script("stop.sh")),
        pre_compact = ts_string_literal(&script("pre-compact.sh")),
        pre_tool = ts_string_literal(&script("pre-tool-use.sh")),
        post_tool = ts_string_literal(&script("post-tool-use.sh")),
    );

    let outcome = apply_atomic(&path, move |_existing| Ok(body.clone()))?;
    println!(
        "✓ {} {} ({})",
        outcome.verb(),
        path.display(),
        match outcome {
            ApplyOutcome::Created => "new plugin file",
            ApplyOutcome::Updated => "backup written next to it",
            ApplyOutcome::NoOp => "already up to date",
        }
    );
    if !matches!(outcome, ApplyOutcome::NoOp) {
        println!();
        println!("OpenCode auto-loads plugins from ~/.config/opencode/plugins/ on next start.");
        println!("If you're already inside an `opencode` session, restart it for the");
        println!("new plugin to take effect.");
    }
    Ok(())
}

/// Emit a TypeScript string literal containing `s`. Escapes
/// backslashes, double quotes, and newlines. Sufficient for the
/// URL + auth-token + path strings we embed; the generated file is
/// not user-edited.
fn ts_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn render_agent(
    label: &str,
    hooks_dir: &Path,
    server_url: &str,
    auth_token: Option<&str>,
) -> Result<()> {
    println!("# {label} hook scripts (manual install — wire each to the matching event)");
    println!("# Hook scripts: {}", hooks_dir.display());
    println!("# AI-memory server URL: {server_url}");
    if auth_token.is_some() {
        println!("# Auth: set AI_MEMORY_AUTH_TOKEN in each hook's environment to the");
        println!("#       value passed via --auth-token (omitted from this printout).");
    } else {
        println!("# Auth: server requires no bearer token. To require one, generate a");
        println!("#       token with `ai-memory generate-auth-token` and pass it via");
        println!("#       --auth-token here AND set AI_MEMORY_AUTH_TOKEN on the server.");
    }
    println!();
    for entry in std::fs::read_dir(hooks_dir)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_file() && p.extension().is_some_and(|e| e == "sh") {
            println!("- {}", p.display());
        }
    }
    println!();
    println!("Set AI_MEMORY_HOOK_URL in each hook's environment to override the default.");
    Ok(())
}

fn resolve_hooks_dir(explicit: Option<&Path>, agent: AgentChoice) -> Result<PathBuf> {
    let sub = match agent {
        AgentChoice::ClaudeCode => "claude-code",
        AgentChoice::Codex => "codex",
        AgentChoice::Cursor => "cursor",
        AgentChoice::GeminiCli => "gemini-cli",
        AgentChoice::OpenCode => "opencode",
        // OpenClaw has no hooks → no script dir needed. Return a
        // sentinel that's never touched; the caller's apply path
        // short-circuits before any filesystem use.
        AgentChoice::Openclaw => return Ok(PathBuf::from("/dev/null")),
    };
    if let Some(p) = explicit {
        let path = p.join(sub);
        if path.is_dir() {
            return Ok(path);
        }
        anyhow::bail!("hooks directory {} does not exist", path.display());
    }

    // Probe candidates in order. The first dir that exists wins.
    let candidates: [PathBuf; 3] = [
        // Cargo-run from the repo.
        repo_root_guess()
            .map(|r| r.join("hooks").join(sub))
            .unwrap_or_default(),
        // Docker image lays them out under /usr/local/share/ai-memory/.
        PathBuf::from(format!("/usr/local/share/ai-memory/hooks/{sub}")),
        // Local install honourable mention.
        dirs::data_local_dir()
            .map(|d| d.join("ai-memory/hooks").join(sub))
            .unwrap_or_default(),
    ];
    for path in &candidates {
        if !path.as_os_str().is_empty() && path.is_dir() {
            return Ok(path.clone());
        }
    }
    anyhow::bail!("could not locate hooks directory. Tried: {:?}", candidates,);
}

fn repo_root_guess() -> Option<PathBuf> {
    // When the binary lives under target/{debug,release}/<name>, the
    // workspace root is two parents up.
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent()?.parent()?.parent().map(Path::to_path_buf))
}

// CLAUDE_CODE_EVENTS + build_claude_code_payload now live in
// `super::render_shared`, shared with `setup-agent`.

fn render_claude_code(hooks_dir: &Path, server_url: &str, auth_token: Option<&str>) -> Result<()> {
    // Soft check: warn (don't bail) if a script is missing. The user
    // may be running this command inside docker against a host path
    // that exists only on the host's filesystem — bailing would
    // sabotage the docker-only flow `setup-agent` enables.
    for (_, script) in super::render_shared::CLAUDE_CODE_EVENTS {
        let abs = hooks_dir.join(script);
        if !abs.exists() {
            eprintln!(
                "# warning: {} not present on this filesystem. \
                 If this command is running inside docker against a \
                 host path, you can ignore this; otherwise extract \
                 the scripts first with `ai-memory setup-agent`.",
                abs.display()
            );
        }
    }
    let payload = build_claude_code_payload(hooks_dir, server_url, auth_token);
    let serialized =
        serde_json::to_string_pretty(&payload).context("serializing claude code hook config")?;
    println!("# Claude Code hook config — merge into ~/.claude/settings.json");
    println!("# Hook scripts: {}", hooks_dir.display());
    println!("# AI-memory server URL: {server_url}");
    if auth_token.is_some() {
        println!("# Auth: AI_MEMORY_AUTH_TOKEN embedded in each hook's env block below.");
        println!("#       Treat ~/.claude/settings.json as sensitive (chmod 600).");
    }
    println!();
    println!("{serialized}");
    Ok(())
}
