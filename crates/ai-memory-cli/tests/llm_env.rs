//! Subprocess smoke tests for LLM configuration via `AI_MEMORY_*` env vars.
//!
//! These tests assert that a bare-minimum deployment (no `config.toml` LLM
//! keys, everything supplied through the environment — exactly the `.env`
//! shipped in the repo) wires through to the running server:
//!
//!   AI_MEMORY_LLM_PROVIDER=gemini
//!   AI_MEMORY_LLM_MODEL=gemini-3.1-flash-lite
//!   GEMINI_API_KEY=<key>          # note: loader strips the quotes in .env
//!   AI_MEMORY_EMBEDDING_DIM=768
//!
//! Each test spawns the `ai-memory` binary with one env-var combination,
//! waits for the startup log line that records the resolved LLM state, and
//! kills the child. This is the only safe place to assert env-var loading
//! that involves the process-global `GEMINI_API_KEY` (the crate forbids
//! `unsafe_code`, so process env can't be cleared between unit tests and a
//! `set_var` here would leak into the gemini-key unit tests).

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Strip ANSI SGR escape sequences (`ESC[...m`) from a log line. The binary's
/// tracing subscriber emits color codes even to a piped stderr, which would
/// otherwise split `provider="gemini"` into `provider[0m[2m="gemini"`.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // consume until 'm' (SGR) or the sequence ends
            for n in chars.by_ref() {
                if n == 'm' {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_ai-memory")
}

/// Spawn the binary with the given env vars and stream stderr until either
/// `needle` appears in a line or `timeout` elapses. Always kills the child
/// before returning. Returns `(matched_line_or_none, all_stderr_captured)`.
fn spawn_and_wait_for_log(
    envs: &[(&str, &str)],
    needle: &str,
    timeout: Duration,
) -> (Option<String>, String) {
    let tmp = TempDir::new().expect("tempdir for serve");
    // `--bind 127.0.0.1:0` asks the OS for any free port — we never want to
    // actually talk to the listener, only to confirm the engine starts up far
    // enough to log the LLM configuration state.
    let mut cmd = Command::new(bin());
    cmd.args([
        "serve",
        "--transport",
        "http",
        "--bind",
        "127.0.0.1:0",
        "--data-dir",
    ])
    .arg(tmp.path())
    .env("AI_MEMORY_DATA_DIR", tmp.path())
    // Force tracing to spit at least info-level so the LLM state line is
    // rendered to stderr.
    .env("RUST_LOG", "info")
    .stdout(Stdio::null())
    .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn ai-memory serve");

    // Pump stderr in a background thread so we can apply a wall-clock timeout
    // on the *match*, not on the underlying read syscalls.
    let stderr = child.stderr.take().expect("stderr piped");
    let (tx, rx) = mpsc::channel::<String>();
    let pump = std::thread::spawn(move || {
        let mut all = String::new();
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let clean = strip_ansi(&line);
            all.push_str(&clean);
            all.push('\n');
            if tx.send(clean).is_err() {
                break;
            }
        }
        all
    });

    let deadline = Instant::now() + timeout;
    let mut matched: Option<String> = None;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(line) => {
                if line.contains(needle) {
                    matched = Some(line);
                    break;
                }
            }
            Err(_) => break,
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    // Pump thread terminates once stderr closes (post-kill); collect what it
    // captured.
    let all_stderr = pump.join().unwrap_or_default();
    (matched, all_stderr)
}

const LLM_ENABLED_NEEDLE: &str = "memory_consolidate + PreCompact LLM checkpointing enabled";
const LLM_UNSET_NEEDLE: &str = "AI_MEMORY_LLM_PROVIDER unset";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(8);

#[test]
fn bare_env_enables_llm_consolidation() {
    // The repo's `.env` shape: provider + model + key + embedding dim all via
    // env. The server must report LLM checkpointing as enabled with the
    // exact provider/model read from the environment.
    let (line, all) = spawn_and_wait_for_log(
        &[
            ("AI_MEMORY_LLM_PROVIDER", "gemini"),
            ("AI_MEMORY_LLM_MODEL", "gemini-3.1-flash-lite"),
            // `.env` writes this as `GEMINI_API_KEY="AQ..."`; a real loader
            // (shell / dotenv) strips the surrounding quotes, so the effective
            // value is the bare key. A placeholder is fine — the provider
            // builds with the key present; it isn't validated at startup.
            ("GEMINI_API_KEY", "AQ.placeholder"),
            ("AI_MEMORY_EMBEDDING_DIM", "768"),
        ],
        LLM_ENABLED_NEEDLE,
        STARTUP_TIMEOUT,
    );
    let line = line.unwrap_or_else(|| panic!("LLM-enabled log line not found.\nstderr:\n{all}"));
    let has_provider = line.contains("provider=\"gemini\"");
    let has_model = line.contains("model=\"gemini-3.1-flash-lite\"");
    assert!(
        has_provider && has_model,
        "LLM env vars must surface. has_provider={has_provider} has_model={has_model}\nGot: {line}"
    );
}

#[test]
fn env_overrides_default_gemini_model() {
    // Setting only the provider (gemini) without a model uses the built-in
    // default model. Setting `AI_MEMORY_LLM_MODEL` must override it — pin this
    // so a future refactor can't silently ignore the env model.
    let (line, all) = spawn_and_wait_for_log(
        &[
            ("AI_MEMORY_LLM_PROVIDER", "gemini"),
            ("AI_MEMORY_LLM_MODEL", "gemini-3.1-flash-lite"),
            ("GEMINI_API_KEY", "AQ-placeholder"),
        ],
        LLM_ENABLED_NEEDLE,
        STARTUP_TIMEOUT,
    );
    let line = line.unwrap_or_else(|| panic!("LLM-enabled log line not found.\nstderr:\n{all}"));
    assert!(
        line.contains("model=\"gemini-3.1-flash-lite\""),
        "explicit AI_MEMORY_LLM_MODEL must win over the gemini default.\nGot: {line}\nfull stderr:\n{all}"
    );
}

#[test]
fn missing_provider_disables_llm_consolidation() {
    // No `AI_MEMORY_LLM_PROVIDER` -> the server must log the "unset"
    // warning and keep running (consolidation simply off). Confirms env
    // presence, not just a hardcoded default, drives the enabled/disabled
    // branch.
    //
    // The test process may inherit a provider from the developer's own
    // `.env` (e.g. `AI_MEMORY_LLM_PROVIDER=gemini`), so we explicitly
    // neutralize it with an empty value rather than relying on a clean env.
    let (line, all) = spawn_and_wait_for_log(
        &[("AI_MEMORY_LLM_PROVIDER", "")],
        LLM_UNSET_NEEDLE,
        STARTUP_TIMEOUT,
    );
    assert!(
        line.is_some(),
        "expected 'AI_MEMORY_LLM_PROVIDER unset' log when provider env is absent.\nstderr:\n{all}"
    );
}
