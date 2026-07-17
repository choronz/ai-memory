//! Subprocess tests for `ai-memory purge-session` local validation.
//!
//! Asserts the one guard that runs *before* any network call: a malformed
//! `--id` fails locally with a clear UUID error (no server contact). The
//! happy path (actual deletion) is covered end-to-end by the
//! `admin_purge_session` integration tests against a real `AdminState`.

use std::process::{Command, Stdio};
use tempfile::TempDir;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_ai-memory")
}

/// Run `purge-session` with the given extra args; return (exit_ok, stderr).
fn run_purge_session(args: &[&str]) -> (bool, String) {
    let tmp = TempDir::new().expect("tempdir for cli");
    let mut cmd = Command::new(bin());
    cmd.args(["purge-session"])
        .args(args)
        // A bogus server URL proves the process bails before contacting it.
        .env("AI_MEMORY_SERVER_URL", "http://127.0.0.1:1/nope")
        .env("AI_MEMORY_DATA_DIR", tmp.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().expect("spawn ai-memory purge-session");
    let output = child.wait_with_output().expect("wait on purge-session");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (output.status.success(), stderr)
}

#[test]
fn purge_session_malformed_id_fails_locally() {
    let (ok, stderr) = run_purge_session(&["--id", "not-a-uuid"]);
    assert!(!ok, "malformed id must not succeed");
    assert!(
        stderr.contains("valid session UUID"),
        "expected UUID validation error, got: {stderr}"
    );
}
