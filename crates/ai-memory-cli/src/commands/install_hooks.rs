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
use serde_json::json;

use crate::cli::{AgentChoice, InstallHooksArgs};
use crate::config::Config;

/// Run the `install-hooks` subcommand.
///
/// # Errors
/// Returns an error if the hook script directory cannot be located.
pub fn run(_config: &Config, args: InstallHooksArgs) -> Result<()> {
    let hooks_dir = resolve_hooks_dir(args.hooks_dir.as_deref(), args.agent)?;
    let auth = args.auth_token.as_deref();
    match args.agent {
        AgentChoice::ClaudeCode => render_claude_code(&hooks_dir, &args.server_url, auth),
        AgentChoice::Codex => render_agent("codex", &hooks_dir, &args.server_url, auth),
        AgentChoice::OpenCode => render_agent("opencode", &hooks_dir, &args.server_url, auth),
    }
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
        AgentChoice::OpenCode => "opencode",
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

/// The seven Claude Code hook events ai-memory wires up, paired with
/// the vendored script filename. Used by both `install-hooks` and
/// `setup-agent`.
pub(crate) const CLAUDE_CODE_EVENTS: [(&str, &str); 7] = [
    ("SessionStart", "session-start.sh"),
    ("UserPromptSubmit", "user-prompt-submit.sh"),
    ("PreToolUse", "pre-tool-use.sh"),
    ("PostToolUse", "post-tool-use.sh"),
    ("PreCompact", "pre-compact.sh"),
    ("Stop", "stop.sh"),
    ("SessionEnd", "session-end.sh"),
];

/// Build the Claude Code settings-fragment JSON for the given paths.
///
/// `emit_root` is the directory the JSON snippet will reference as
/// the home of the hook scripts. It does NOT need to exist on the
/// filesystem this binary is running on — that's exactly the docker
/// case where `setup-agent` writes to a mounted volume and emits
/// host-side paths.
///
/// `auth_token`, when set, lands in each hook's `env` block as
/// `AI_MEMORY_AUTH_TOKEN`, which the shell scripts forward as
/// `Authorization: Bearer …` to the server.
pub(crate) fn build_claude_code_payload(
    emit_root: &Path,
    server_url: &str,
    auth_token: Option<&str>,
) -> serde_json::Value {
    let mut hooks_block = serde_json::Map::new();
    for (event, script) in CLAUDE_CODE_EVENTS {
        let abs = emit_root.join(script);
        let mut env = serde_json::Map::new();
        env.insert(
            "AI_MEMORY_HOOK_URL".into(),
            serde_json::Value::String(server_url.to_string()),
        );
        if let Some(t) = auth_token {
            env.insert(
                "AI_MEMORY_AUTH_TOKEN".into(),
                serde_json::Value::String(t.to_string()),
            );
        }
        hooks_block.insert(
            event.into(),
            json!([{
                "command": abs.to_string_lossy().into_owned(),
                "env": env,
            }]),
        );
    }
    json!({ "hooks": hooks_block })
}

fn render_claude_code(hooks_dir: &Path, server_url: &str, auth_token: Option<&str>) -> Result<()> {
    // Soft check: warn (don't bail) if a script is missing. The user
    // may be running this command inside docker against a host path
    // that exists only on the host's filesystem — bailing would
    // sabotage the docker-only flow `setup-agent` enables.
    for (_, script) in CLAUDE_CODE_EVENTS {
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
