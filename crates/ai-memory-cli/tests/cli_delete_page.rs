//! Subprocess tests for `ai-memory delete-page`.
//!
//! `delete-page` is scoped to a single page (no `--confirm` required, unlike
//! `purge-project`), so the only local guard is project resolution, which
//! needs a live server. The happy path + 404 are covered end-to-end by the
//! `admin_delete_page` integration tests against a real `AdminState`. This
//! file currently holds no standalone CLI assertions beyond a smoke check that
//! the binary accepts the `--path` flag without contacting a server.

use std::process::{Command, Stdio};
use tempfile::TempDir;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_ai-memory")
}

#[test]
fn delete_page_accepts_path_without_confirm() {
    let tmp = TempDir::new().expect("tempdir for cli");
    let mut cmd = Command::new(bin());
    cmd.args(["delete-page", "--path", "notes/doomed.md"])
        // A bogus server URL: the flag parses locally; the network call is not
        // asserted here (covered by admin_delete_page integration tests).
        .env("AI_MEMORY_SERVER_URL", "http://127.0.0.1:1/nope")
        .env("AI_MEMORY_DATA_DIR", tmp.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().expect("spawn ai-memory delete-page");
    let output = child.wait_with_output().expect("wait on delete-page");
    // The process must not fail at argument-parsing (no `--confirm` needed).
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        !stderr.contains("unexpected argument") && !stderr.contains("requires --confirm"),
        "delete-page must not require --confirm: {stderr}"
    );
}
