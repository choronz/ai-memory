//! `ai-memory setup-agent` — one-shot agent integration for the
//! docker-primary workflow.
//!
//! Solves the problem that `install-hooks` alone can't handle in a
//! docker-only deploy: the JSON snippet `install-hooks` emits
//! references absolute paths to hook scripts, and those paths must
//! exist on the host machine that runs the agent CLI (Claude Code et
//! al. shell out from the host, not inside the container).
//!
//! `setup-agent` bundles the extract + render into one command:
//!
//!     docker run --rm \
//!       -v "$HOME/.ai-memory:/host" \
//!       akitaonrails/ai-memory:latest \
//!       setup-agent \
//!         --agent claude-code \
//!         --to /host/hooks \
//!         --host-prefix "$HOME/.ai-memory/hooks" \
//!         --auth-token "$TOKEN"
//!
//! 1. Copies `/usr/local/share/ai-memory/hooks/claude-code/*.sh` into
//!    `/host/hooks/claude-code/` (which on the host is
//!    `$HOME/.ai-memory/hooks/claude-code/`).
//! 2. Prints the JSON config snippet whose `command` fields point at
//!    `$HOME/.ai-memory/hooks/claude-code/*.sh` (via `--host-prefix`)
//!    so Claude Code on the host can exec them.
//!
//! When `--host-prefix` is omitted it defaults to `--to`, which is
//! the right behaviour for a non-docker (`cargo run`) invocation
//! where the in-container path and the host path are the same thing.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::cli::{AgentChoice, SetupAgentArgs};
use crate::commands::render_shared::{CLAUDE_CODE_EVENTS, build_claude_code_payload};
use crate::config::Config;

/// Run the `setup-agent` subcommand.
///
/// # Errors
/// Returns an error if the source bundle can't be located, the
/// destination directory can't be created, any script copy fails,
/// or the JSON config can't be serialised.
pub fn run(_config: &Config, args: SetupAgentArgs) -> Result<()> {
    let agent_sub = match args.agent {
        AgentChoice::ClaudeCode => "claude-code",
        AgentChoice::Codex => "codex",
        AgentChoice::Cursor => "cursor",
        AgentChoice::GeminiCli => "gemini-cli",
        AgentChoice::OpenCode => "opencode",
        AgentChoice::Openclaw => {
            anyhow::bail!(
                "OpenClaw has no lifecycle hooks (only HTTP webhooks); \
                 `setup-agent --agent openclaw` is not applicable. Use \
                 `install-mcp --client openclaw --apply` for the MCP config \
                 only."
            );
        }
    };

    let source = resolve_source(args.source.as_deref(), agent_sub)?;
    let dest_dir = args.to.join(agent_sub);

    fs::create_dir_all(&dest_dir)
        .with_context(|| format!("creating destination {}", dest_dir.display()))?;

    let mut copied = 0_usize;
    for entry in fs::read_dir(&source)
        .with_context(|| format!("reading source bundle {}", source.display()))?
    {
        let entry = entry?;
        let from = entry.path();
        if !from.is_file() || from.extension().and_then(|s| s.to_str()) != Some("sh") {
            continue;
        }
        let file_name = from
            .file_name()
            .with_context(|| format!("invalid hook script path {}", from.display()))?;
        let to = dest_dir.join(file_name);
        fs::copy(&from, &to)
            .with_context(|| format!("copying {} → {}", from.display(), to.display()))?;
        // Preserve executable bit so the agent CLI can actually run
        // the scripts. On Windows this is a no-op.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&to)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&to, perms)?;
        }
        copied += 1;
    }

    eprintln!(
        "✓ Extracted {copied} hook script(s) from {} to {}",
        source.display(),
        dest_dir.display(),
    );

    // The path the rendered JSON should reference. Defaults to where
    // we just copied the scripts; override with --host-prefix when
    // running inside docker against a mounted volume.
    let emit_root = args
        .host_prefix
        .as_deref()
        .unwrap_or(&args.to)
        .join(agent_sub);

    match args.agent {
        AgentChoice::ClaudeCode => emit_claude_code(&emit_root, &args)?,
        AgentChoice::Codex | AgentChoice::Cursor | AgentChoice::GeminiCli | AgentChoice::OpenCode => {
            emit_other(&emit_root, agent_sub, &args);
        }
        AgentChoice::Openclaw => {
            // Unreachable — the early bail at the top of run()
            // catches openclaw before we get here. Defensive
            // arm so the match stays exhaustive if the bail is
            // ever removed.
            unreachable!("openclaw handled by the early bail above");
        }
    }
    Ok(())
}

fn emit_claude_code(emit_root: &Path, args: &SetupAgentArgs) -> Result<()> {
    let payload =
        build_claude_code_payload(emit_root, &args.server_url, args.auth_token.as_deref());
    let serialized =
        serde_json::to_string_pretty(&payload).context("serializing Claude Code hook config")?;
    println!("# Claude Code — merge into ~/.claude/settings.json");
    println!("# Hook scripts (must be reachable from the host that runs Claude Code):");
    println!("#   {}", emit_root.display());
    println!("# AI-memory server: {}", args.server_url);
    if args.auth_token.is_some() {
        println!("# Auth: AI_MEMORY_AUTH_TOKEN embedded in each hook's env block.");
        println!("#       Treat ~/.claude/settings.json as sensitive (chmod 600).");
    }
    println!("# Tip: also run `ai-memory install-mcp --client claude-code --auth-token <…>`");
    println!("#      to register the MCP endpoint (separate from hooks).");
    println!();
    println!("{serialized}");
    Ok(())
}

fn emit_other(emit_root: &Path, label: &str, args: &SetupAgentArgs) {
    // Codex + OpenCode hook configs vary by version; we just point at
    // the extracted scripts and let the user wire them up. Future
    // versions can render structured JSON/TOML once those formats
    // settle upstream.
    println!("# {label} hook scripts (manual wire-up — formats still evolving upstream)");
    println!("# Scripts located at: {}", emit_root.display());
    println!("# Server URL:         {}", args.server_url);
    if args.auth_token.is_some() {
        println!("# Auth: set AI_MEMORY_AUTH_TOKEN in each hook's environment to the");
        println!("#       value you passed via --auth-token (not echoed).");
    }
    println!();
    for (_, script) in CLAUDE_CODE_EVENTS {
        println!("- {}", emit_root.join(script).display());
    }
    println!();
    println!("Set AI_MEMORY_HOOK_URL in each hook's environment to override the default.");
    println!("Also run `ai-memory install-mcp --client {label}` to wire MCP separately.");
}

fn resolve_source(explicit: Option<&Path>, sub: &str) -> Result<PathBuf> {
    let candidates: Vec<PathBuf> = if let Some(p) = explicit {
        vec![p.join(sub)]
    } else {
        let mut v = vec![
            // Docker image lays them out under /usr/local/share/.
            PathBuf::from(format!("/usr/local/share/ai-memory/hooks/{sub}")),
        ];
        // Repo-local fallback for `cargo run setup-agent` during dev.
        if let Some(p) = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent()?.parent()?.parent().map(Path::to_path_buf))
        {
            v.push(p.join("hooks").join(sub));
        }
        v
    };
    for path in &candidates {
        if path.is_dir() {
            return Ok(path.clone());
        }
    }
    bail!(
        "could not locate hook source bundle for {sub}. \
         Tried: {candidates:?}. Pass --source <dir> to override."
    );
}
