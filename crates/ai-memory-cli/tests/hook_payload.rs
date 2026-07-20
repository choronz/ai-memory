//! Native hook stdin parsing regressions, including PowerShell's UTF-8 BOM.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_ai-memory")
}

fn run_hook(data_dir: &Path, payload: &[u8]) -> Output {
    let mut child = Command::new(bin())
        .args(["--data-dir"])
        .arg(data_dir)
        .args([
            "hook",
            "--event",
            "pre-tool-use",
            "--agent",
            "claude-code",
            "--server-url",
            "http://127.0.0.1:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn native hook");
    child
        .stdin
        .take()
        .expect("hook stdin")
        .write_all(payload)
        .expect("write hook payload");
    child.wait_with_output().expect("wait for native hook")
}

fn spooled_body(data_dir: &Path) -> String {
    let spool = data_dir.join("hook-spool");
    let entries = std::fs::read_dir(spool)
        .expect("hook spool")
        .collect::<Result<Vec<_>, _>>()
        .expect("spool entries");
    assert_eq!(entries.len(), 1);
    let entry: serde_json::Value =
        serde_json::from_slice(&std::fs::read(entries[0].path()).expect("read spool entry"))
            .expect("parse spool entry");
    entry["body"].as_str().expect("spooled body").to_owned()
}

#[test]
fn native_hook_accepts_plain_and_bom_prefixed_json() {
    let payload = br#"{"session_id":"windows-test","cwd":"C:\\dev\\project","tool_name":"Read","tool_input":{"file_path":"README.md"}}"#;

    for with_bom in [false, true] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut stdin = Vec::new();
        if with_bom {
            stdin.extend_from_slice(&[0xef, 0xbb, 0xbf]);
        }
        stdin.extend_from_slice(payload);

        let output = run_hook(tmp.path(), &stdin);
        assert!(output.status.success());
        assert_eq!(output.stdout, b"{}\n");
        assert!(
            output.stderr.is_empty(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(spooled_body(tmp.path()).as_bytes(), payload);
    }
}

#[test]
fn malformed_native_hook_payload_warns_without_leaking_or_spooling() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = run_hook(
        tmp.path(),
        b"\xef\xbb\xbf{\"secret\":\"SENTINEL_PRIVATE_PAYLOAD\"",
    );

    assert!(output.status.success());
    assert_eq!(output.stdout, b"{}\n");
    let stderr = String::from_utf8(output.stderr).expect("stderr utf8");
    assert_eq!(
        stderr,
        "ai-memory hook warning: could not parse event payload as JSON; nothing was captured\n"
    );
    assert!(!stderr.contains("SENTINEL_PRIVATE_PAYLOAD"));
    assert!(!tmp.path().join("hook-spool").exists());
}
