//! Generated OpenClaw lifecycle plugin support.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

use crate::cli::InstallHooksArgs;
use crate::commands::apply_shared::{ApplyOutcome, apply_atomic};
use crate::commands::render_shared::ts_string_literal;

const PLUGIN_ID: &str = "ai-memory";
const PACKAGE_NAME: &str = "@ai-memory/openclaw-plugin";
const PACKAGE_JSON: &str = "package.json";
const MANIFEST_JSON: &str = "openclaw.plugin.json";
const ENTRYPOINT_TS: &str = "index.ts";
const OPENCLAW_BIN: &str = "openclaw";

/// Write and install the generated OpenClaw plugin package.
pub(crate) fn apply(
    server_url: &str,
    auth_token: Option<&str>,
    args: &InstallHooksArgs,
) -> Result<()> {
    let plugin_dir = resolve_plugin_dir(args)?;
    let outcomes = write_package(&plugin_dir, server_url, auth_token)?;
    for (path, outcome) in &outcomes {
        println!(
            "✓ {} {} ({})",
            outcome.verb(),
            path.display(),
            outcome_detail(*outcome)
        );
    }

    match install_plugin(&plugin_dir)? {
        InstallStatus::Installed => {
            println!();
            println!("OpenClaw plugin installed from {}.", plugin_dir.display());
            println!(
                "If your OpenClaw gateway did not auto-restart, run `openclaw gateway restart`."
            );
            println!("Verify with `openclaw plugins inspect ai-memory --runtime --json`.");
        }
        InstallStatus::CliMissing => {
            println!();
            println!("OpenClaw CLI not found on PATH; plugin package was written only.");
            println!("Install it with:");
            println!(
                "  openclaw plugins install --link {} --force",
                plugin_dir.display()
            );
            println!("  openclaw gateway restart");
            println!("  openclaw plugins inspect ai-memory --runtime --json");
        }
    }
    Ok(())
}

/// Print the generated package for manual installation.
pub(crate) fn render(server_url: &str, auth_token: Option<&str>) {
    println!("# OpenClaw native plugin package");
    println!("# Re-run with `--apply` to write the package and call:");
    println!("#   openclaw plugins install --link <package-dir> --force");
    println!("# OpenClaw loads plugin code at gateway startup; restart if your");
    println!("# managed gateway does not auto-restart after install.");
    println!();
    println!("## {PACKAGE_JSON}");
    println!("{}", package_json());
    println!("## {MANIFEST_JSON}");
    println!("{}", manifest_json());
    println!("## {ENTRYPOINT_TS}");
    println!("{}", build_plugin(server_url, auth_token));
}

fn outcome_detail(outcome: ApplyOutcome) -> &'static str {
    match outcome {
        ApplyOutcome::Created => "new file",
        ApplyOutcome::Updated => "backup written next to it",
        ApplyOutcome::NoOp => "already up to date",
    }
}

fn resolve_plugin_dir(args: &InstallHooksArgs) -> Result<PathBuf> {
    if let Some(path) = &args.config_file {
        return Ok(path.clone());
    }
    Ok(dirs::data_local_dir()
        .context("could not locate the user data-local directory")?
        .join("ai-memory")
        .join("openclaw-plugin"))
}

fn write_package(
    plugin_dir: &Path,
    server_url: &str,
    auth_token: Option<&str>,
) -> Result<Vec<(PathBuf, ApplyOutcome)>> {
    let files = [
        (PACKAGE_JSON, package_json()),
        (MANIFEST_JSON, manifest_json()),
        (ENTRYPOINT_TS, build_plugin(server_url, auth_token)),
    ];
    let mut outcomes = Vec::with_capacity(files.len());
    for (name, body) in files {
        let path = plugin_dir.join(name);
        let outcome = apply_atomic(&path, move |_existing| Ok(body.clone()))?;
        outcomes.push((path, outcome));
    }
    Ok(outcomes)
}

enum InstallStatus {
    Installed,
    CliMissing,
}

fn install_plugin(plugin_dir: &Path) -> Result<InstallStatus> {
    let output = match Command::new(OPENCLAW_BIN)
        .args(["plugins", "install", "--link"])
        .arg(plugin_dir)
        .arg("--force")
        .output()
    {
        Ok(output) => output,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(InstallStatus::CliMissing),
        Err(e) => return Err(e).context("running openclaw plugins install"),
    };
    if !output.status.success() {
        anyhow::bail!(
            "openclaw plugins install failed with status {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let enable = Command::new(OPENCLAW_BIN)
        .args(["plugins", "enable", PLUGIN_ID])
        .output()
        .context("running openclaw plugins enable")?;
    if !enable.status.success() {
        eprintln!(
            "# warning: `openclaw plugins enable ai-memory` exited with {}\n# stdout:\n{}\n# stderr:\n{}",
            enable.status,
            String::from_utf8_lossy(&enable.stdout),
            String::from_utf8_lossy(&enable.stderr)
        );
    }

    Ok(InstallStatus::Installed)
}

fn package_json() -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "name": PACKAGE_NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "private": true,
        "type": "module",
        "openclaw": {
            "extensions": [format!("./{ENTRYPOINT_TS}")]
        }
    }))
    .expect("OpenClaw package metadata serializes")
        + "\n"
}

fn manifest_json() -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "id": PLUGIN_ID,
        "name": "ai-memory",
        "description": "Capture OpenClaw session lifecycle, tool use, compaction, and handoffs into ai-memory.",
        "activation": {
            "onCapabilities": ["hook"]
        },
        "configSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {}
        }
    }))
    .expect("OpenClaw manifest serializes")
        + "\n"
}

fn build_plugin(server_url: &str, auth_token: Option<&str>) -> String {
    let token_line = auth_token
        .map(|t| format!("const TOKEN: string | null = {};\n", ts_string_literal(t)))
        .unwrap_or_else(|| "const TOKEN: string | null = null;\n".to_string());
    format!(
        r#"// Auto-generated by `ai-memory install-hooks --agent openclaw --apply`.
// Edit by re-running the command, not by hand. install-hooks owns
// this local OpenClaw plugin package.

import {{ definePluginEntry }} from "openclaw/plugin-sdk/plugin-entry";

const SERVER = {server_literal}.replace(/\/+$/, "");
const AGENT = "openclaw";
{token_line}

function timeoutSignal(ms: number): AbortSignal | undefined {{
  if (typeof AbortSignal === "undefined") return undefined;
  const factory = (AbortSignal as unknown as {{ timeout?: (ms: number) => AbortSignal }}).timeout;
  return factory ? factory(ms) : undefined;
}}

function authHeaders(): Record<string, string> {{
  return TOKEN ? {{ Authorization: `Bearer ${{TOKEN}}` }} : {{}};
}}

function textFrom(value: unknown): string {{
  if (value === null || value === undefined) return "";
  if (typeof value === "string") return value;
  if (Array.isArray(value)) return value.map(textFrom).filter(Boolean).join("\n\n").trim();
  const obj = value as any;
  if (typeof obj.text === "string") return obj.text;
  if (typeof obj.content === "string") return obj.content;
  if (typeof obj.prompt === "string") return obj.prompt;
  try {{
    return JSON.stringify(value);
  }} catch (_e) {{
    return String(value);
  }}
}}

function sessionID(event: any, ctx: any): string | undefined {{
  const value = ctx?.sessionId ?? ctx?.sessionID ?? ctx?.sessionKey ?? event?.sessionId ?? event?.sessionID ?? event?.sessionKey;
  return typeof value === "string" && value.length > 0 ? value : undefined;
}}

function cwd(event: any, ctx: any): string | undefined {{
  const value = ctx?.workspaceDir ?? ctx?.cwd ?? event?.cwd ?? event?.workspaceDir;
  return typeof value === "string" && value.length > 0 ? value : undefined;
}}

function payload(event: any, ctx: any, extra: Record<string, unknown> = {{}}): Record<string, unknown> {{
  return {{
    sessionID: sessionID(event, ctx),
    cwd: cwd(event, ctx),
    agentID: ctx?.agentId,
    runID: ctx?.runId ?? event?.runId,
    jobID: ctx?.jobId,
    ...extra,
  }};
}}

const startedSessions = new Set<string>();
const handoffChecked = new Set<string>();
const preCompactLast = new Map<string, number>();

function rememberSession(event: any, ctx: any): void {{
  const id = sessionID(event, ctx);
  if (!id || startedSessions.has(id)) return;
  startedSessions.add(id);
  postHook("session-start", payload(event, ctx, {{ reason: event?.reason }}));
}}

function postPreCompact(event: any, ctx: any): void {{
  rememberSession(event, ctx);
  const key = sessionID(event, ctx) || "unknown";
  const now = Date.now();
  const last = preCompactLast.get(key) ?? 0;
  if (now - last < 1000) return;
  preCompactLast.set(key, now);
  postHook("pre-compact", payload(event, ctx, {{ reason: event?.reason }}));
}}

function postHook(eventName: string, body: Record<string, unknown>): void {{
  const url = new URL(`${{SERVER}}/hook`);
  url.searchParams.set("event", eventName);
  url.searchParams.set("agent", AGENT);
  try {{
    void fetch(url, {{
      method: "POST",
      headers: {{ "Content-Type": "application/json", ...authHeaders() }},
      body: JSON.stringify(body),
      signal: timeoutSignal(500),
    }}).catch(() => undefined);
  }} catch (_e) {{
    // Fire-and-forget. Hooks must never block OpenClaw.
  }}
}}

async function fetchHandoff(event: any, ctx: any): Promise<string | undefined> {{
  const currentCwd = cwd(event, ctx);
  if (!currentCwd) return undefined;
  const url = new URL(`${{SERVER}}/handoff`);
  url.searchParams.set("agent", AGENT);
  url.searchParams.set("cwd", currentCwd);
  try {{
    const response = await fetch(url, {{
      headers: authHeaders(),
      signal: timeoutSignal(1000),
    }});
    const text = (await response.text()).trim();
    return text.length > 0 ? text : undefined;
  }} catch (_e) {{
    return undefined;
  }}
}}

export default definePluginEntry({{
  id: "ai-memory",
  name: "ai-memory",
  description: "Capture OpenClaw lifecycle events into ai-memory.",
  register(api) {{
    api.on("session_start", (event: any, ctx: any) => {{
      rememberSession(event, ctx);
    }});

    api.on("session_end", (event: any, ctx: any) => {{
      rememberSession(event, ctx);
      postHook("session-end", payload(event, ctx, {{ reason: event?.reason }}));
    }});

    api.on("before_prompt_build", async (event: any, ctx: any) => {{
      rememberSession(event, ctx);
      postHook("user-prompt", payload(event, ctx, {{
        prompt: textFrom(event?.prompt ?? event?.userPrompt ?? event?.message ?? event?.messages?.at?.(-1)),
      }}));

      const id = sessionID(event, ctx);
      if (!id || handoffChecked.has(id)) return;
      handoffChecked.add(id);
      const handoff = await fetchHandoff(event, ctx);
      return handoff ? {{ prependContext: handoff }} : undefined;
    }});

    api.on("before_tool_call", (event: any, ctx: any) => {{
      rememberSession(event, ctx);
      postHook("pre-tool-use", payload(event, ctx, {{
        tool: event?.toolName,
        toolKind: event?.toolKind,
        callID: event?.toolCallId,
        args: event?.params,
      }}));
    }});

    api.on("after_tool_call", (event: any, ctx: any) => {{
      rememberSession(event, ctx);
      postHook("post-tool-use", payload(event, ctx, {{
        tool: event?.toolName,
        toolKind: event?.toolKind,
        callID: event?.toolCallId,
        args: event?.params,
        output: textFrom(event?.result ?? event?.output ?? event?.content),
        error: event?.error,
        durationMs: event?.durationMs,
      }}));
    }});

    api.on("before_compaction", (event: any, ctx: any) => {{
      postPreCompact(event, ctx);
    }});

    api.on("agent_end", (event: any, ctx: any) => {{
      rememberSession(event, ctx);
      postHook("stop", payload(event, ctx, {{ success: event?.success }}));
    }});
  }},
}});
"#,
        server_literal = ts_string_literal(server_url),
        token_line = token_line,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn package_has_manifest_and_hook_entrypoint() {
        let package = package_json();
        let manifest = manifest_json();
        let plugin = build_plugin("http://127.0.0.1:49374", Some("tok"));

        assert!(package.contains(r#""extensions""#));
        assert!(package.contains(r#""./index.ts""#));
        assert!(manifest.contains(r#""id": "ai-memory""#));
        assert!(manifest.contains(r#""onCapabilities""#));
        assert!(manifest.contains(r#""hook""#));
        assert!(plugin.contains("definePluginEntry"));
        assert!(plugin.contains("api.on(\"session_start\""));
        assert!(plugin.contains("api.on(\"session_end\""));
        assert!(plugin.contains("api.on(\"before_prompt_build\""));
        assert!(plugin.contains("api.on(\"before_tool_call\""));
        assert!(plugin.contains("api.on(\"after_tool_call\""));
        assert!(plugin.contains("api.on(\"before_compaction\""));
        assert!(plugin.contains("api.on(\"agent_end\""));
        assert!(plugin.contains("postHook(\"session-start\""));
        assert!(plugin.contains("postHook(\"user-prompt\""));
        assert!(plugin.contains("fetchHandoff"));
        assert!(plugin.contains("prependContext: handoff"));
        assert!(plugin.contains("Bearer ${TOKEN}"));
        assert!(plugin.contains("tok"));
    }

    #[test]
    fn package_writes_all_required_files() {
        let tmp = TempDir::new().unwrap();
        let outcomes = write_package(tmp.path(), "http://127.0.0.1:49374", None).unwrap();

        assert_eq!(outcomes.len(), 3);
        assert!(tmp.path().join(PACKAGE_JSON).is_file());
        assert!(tmp.path().join(MANIFEST_JSON).is_file());
        assert!(tmp.path().join(ENTRYPOINT_TS).is_file());
        assert!(
            std::fs::read_to_string(tmp.path().join(ENTRYPOINT_TS))
                .unwrap()
                .contains("const TOKEN: string | null = null;")
        );
    }
}
