//! End-to-end: install hooks into a temp HOME, then uninstall, and
//! assert the file round-trips (our entries gone, third-party intact).

use std::process::Command;
use std::sync::{Mutex, MutexGuard};

static CLI_TEST_LOCK: Mutex<()> = Mutex::new(());

fn cli_test_lock() -> MutexGuard<'static, ()> {
    CLI_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_ai-memory")
}

#[test]
fn install_then_uninstall_round_trip_claude_hooks() {
    let _guard = cli_test_lock();
    let home = tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    // Pre-seed a third-party hook we must NOT touch.
    std::fs::write(
        claude.join("settings.json"),
        r#"{"hooks":{"Notification":[{"matcher":"","hooks":[{"type":"command","command":"/usr/bin/n.sh"}]}]}}"#,
    )
    .unwrap();

    // Install ai-memory hooks for Claude Code.
    let status = Command::new(bin())
        .args(["install-hooks", "--agent", "claude-code", "--apply"])
        .env("HOME", home.path())
        .env("XDG_DATA_HOME", home.path().join(".local/share"))
        .env("AI_MEMORY_DATA_DIR", home.path().join(".ai-memory-data"))
        .status()
        .unwrap();
    assert!(status.success(), "install-hooks failed");

    // Uninstall (hooks only) and verify.
    let status = Command::new(bin())
        .args(["uninstall", "--apply", "--only", "hooks", "--yes"])
        .env("HOME", home.path())
        .env("XDG_DATA_HOME", home.path().join(".local/share"))
        .env("AI_MEMORY_DATA_DIR", home.path().join(".ai-memory-data"))
        .status()
        .unwrap();
    assert!(status.success(), "uninstall failed");

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(claude.join("settings.json")).unwrap())
            .unwrap();
    // Third-party hook survived.
    assert!(after["hooks"]["Notification"].is_array());
    // None of our events remain.
    for ours in [
        "SessionStart",
        "SessionEnd",
        "PreToolUse",
        "PostToolUse",
        "Stop",
        "PreCompact",
        "UserPromptSubmit",
    ] {
        assert!(
            after["hooks"].get(ours).is_none(),
            "{ours} should be removed"
        );
    }
}

#[test]
fn uninstall_apply_is_idempotent() {
    let _guard = cli_test_lock();
    let home = tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    std::fs::write(
        claude.join("settings.json"),
        r#"{"hooks":{"Stop":[{"matcher":"","hooks":[{"type":"command","command":"AI_MEMORY_HOOK_URL=http://h /x/stop.sh"}]}]}}"#,
    )
    .unwrap();

    let run = || {
        std::process::Command::new(bin())
            .args(["uninstall", "--apply", "--only", "hooks", "--yes"])
            .env("HOME", home.path())
            .env("AI_MEMORY_DATA_DIR", home.path().join(".ai-memory-data"))
            .status()
            .unwrap()
    };

    assert!(run().success(), "first uninstall");
    // Count backups after first run.
    let count_baks = || {
        std::fs::read_dir(&claude)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".bak-"))
            .count()
    };
    let after_first = count_baks();
    assert!(run().success(), "second uninstall (idempotent)");
    assert_eq!(
        count_baks(),
        after_first,
        "second run must not create a new backup"
    );
}

#[test]
fn only_hooks_preserves_mcp_in_same_file() {
    let _guard = cli_test_lock();
    // Gemini-style: hooks + mcpServers in one settings.json.
    let home = tempfile::tempdir().unwrap();
    let gem = home.path().join(".gemini");
    std::fs::create_dir_all(&gem).unwrap();
    std::fs::write(
        gem.join("settings.json"),
        r#"{"hooks":{"SessionStart":[{"matcher":"","hooks":[{"type":"command","command":"AI_MEMORY_HOOK_URL=http://h /x/session-start.sh"}]}]},"mcpServers":{"ai-memory":{"httpUrl":"http://127.0.0.1:49374/mcp"}}}"#,
    )
    .unwrap();

    let status = std::process::Command::new(bin())
        .args(["uninstall", "--apply", "--only", "hooks", "--yes"])
        .env("HOME", home.path())
        .env("AI_MEMORY_DATA_DIR", home.path().join(".ai-memory-data"))
        .status()
        .unwrap();
    assert!(status.success());

    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(gem.join("settings.json")).unwrap()).unwrap();
    // Hooks removed...
    assert!(
        v["hooks"].get("SessionStart").is_none(),
        "hook should be removed"
    );
    // ...but the MCP entry must SURVIVE because --only hooks.
    assert!(
        v["mcpServers"].get("ai-memory").is_some(),
        "--only hooks must NOT touch mcpServers"
    );
}

#[test]
fn uninstall_preserves_user_opencode_plugin_at_ai_memory_path() {
    let _guard = cli_test_lock();
    let home = tempfile::tempdir().unwrap();
    let plugins = home.path().join(".config/opencode/plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    let plugin = plugins.join("ai-memory.ts");
    let original = "// user-owned plugin that happens to use this filename\nexport default {};\n";
    std::fs::write(&plugin, original).unwrap();

    let status = Command::new(bin())
        .args(["uninstall", "--apply", "--only", "hooks", "--yes"])
        .env("HOME", home.path())
        .env("AI_MEMORY_DATA_DIR", home.path().join(".ai-memory-data"))
        .status()
        .unwrap();
    assert!(status.success(), "uninstall failed");

    assert_eq!(std::fs::read_to_string(&plugin).unwrap(), original);
}

#[test]
fn uninstall_deletes_generated_opencode_plugin_only() {
    let _guard = cli_test_lock();
    let home = tempfile::tempdir().unwrap();
    let plugins = home.path().join(".config/opencode/plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    let plugin = plugins.join("ai-memory.ts");
    std::fs::write(
        &plugin,
        "// Auto-generated by `ai-memory install-hooks --agent opencode --apply`.\nconst AGENT = \"open-code\";\n",
    )
    .unwrap();
    let sibling = plugins.join("other.ts");
    std::fs::write(&sibling, "keep me\n").unwrap();

    let status = Command::new(bin())
        .args(["uninstall", "--apply", "--only", "hooks", "--yes"])
        .env("HOME", home.path())
        .env("AI_MEMORY_DATA_DIR", home.path().join(".ai-memory-data"))
        .status()
        .unwrap();
    assert!(status.success(), "uninstall failed");

    assert!(!plugin.exists(), "generated plugin should be deleted");
    assert!(sibling.exists(), "unrelated plugin must be preserved");
}

#[test]
fn uninstall_omp_extension_deletes_only_generated_file() {
    let _guard = cli_test_lock();
    let home = tempfile::tempdir().unwrap();
    let extensions = home.path().join(".omp/agent/extensions");
    std::fs::create_dir_all(&extensions).unwrap();
    let extension = extensions.join("ai-memory.ts");
    let user_content = "// user-owned extension that happens to use this filename\n";
    std::fs::write(&extension, user_content).unwrap();

    let status = Command::new(bin())
        .args(["uninstall", "--apply", "--only", "hooks", "--yes"])
        .env("HOME", home.path())
        .env("AI_MEMORY_DATA_DIR", home.path().join(".ai-memory-data"))
        .status()
        .unwrap();
    assert!(status.success(), "uninstall failed");
    assert_eq!(std::fs::read_to_string(&extension).unwrap(), user_content);

    std::fs::write(
        &extension,
        "// Auto-generated by `ai-memory install-hooks --agent omp --apply`.\nconst AGENT = \"omp\";\n",
    )
    .unwrap();

    let status = Command::new(bin())
        .args(["uninstall", "--apply", "--only", "hooks", "--yes"])
        .env("HOME", home.path())
        .env("AI_MEMORY_DATA_DIR", home.path().join(".ai-memory-data"))
        .status()
        .unwrap();
    assert!(status.success(), "uninstall failed");
    assert!(!extension.exists(), "generated extension should be deleted");
}

#[test]
fn uninstall_preserves_user_openclaw_package_at_ai_memory_path() {
    let _guard = cli_test_lock();
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join(".local/share");
    let plugin_dir = data.join("ai-memory/openclaw-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    let package = plugin_dir.join("package.json");
    let original = r#"{"name":"@ai-memory/openclaw-plugin","private":true}"#;
    std::fs::write(&package, original).unwrap();

    let status = Command::new(bin())
        .args(["uninstall", "--apply", "--only", "hooks", "--yes"])
        .env("HOME", home.path())
        .env("XDG_DATA_HOME", &data)
        .env("AI_MEMORY_DATA_DIR", home.path().join(".ai-memory-data"))
        .status()
        .unwrap();
    assert!(status.success(), "uninstall failed");

    assert_eq!(std::fs::read_to_string(&package).unwrap(), original);
}

#[test]
fn uninstall_antigravity_hooks_preserves_user_entries() {
    let _guard = cli_test_lock();
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join(".gemini/config");
    std::fs::create_dir_all(&config).unwrap();
    let hooks = config.join("hooks.json");
    std::fs::write(
        &hooks,
        r#"{
          "ai-memory": {
            "PreInvocation": [
              {"type":"command","command":"AI_MEMORY_HOOK_URL=http://h /x/session-start.sh"},
              {"type":"command","command":"/usr/bin/user-pre-invocation"}
            ],
            "Stop": [
              {"type":"command","command":"AI_MEMORY_HOOK_URL=http://h /x/stop.sh"}
            ]
          },
          "other-group": {
            "Stop": [{"type":"command","command":"/usr/bin/other"}]
          }
        }"#,
    )
    .unwrap();

    let status = Command::new(bin())
        .args(["uninstall", "--apply", "--only", "hooks", "--yes"])
        .env("HOME", home.path())
        .env("AI_MEMORY_DATA_DIR", home.path().join(".ai-memory-data"))
        .status()
        .unwrap();
    assert!(status.success(), "uninstall failed");

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&hooks).unwrap()).unwrap();
    assert_eq!(
        after["ai-memory"]["PreInvocation"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "third-party entry in same group/event must survive"
    );
    assert!(after["ai-memory"].get("Stop").is_none());
    assert!(after.get("other-group").is_some());
}

#[test]
fn uninstall_mcp_custom_url_removes_antigravity_only_by_endpoint() {
    let _guard = cli_test_lock();
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join(".gemini/antigravity-cli");
    std::fs::create_dir_all(&config).unwrap();
    let mcp = config.join("mcp_config.json");
    std::fs::write(
        &mcp,
        r#"{
          "mcpServers": {
            "ai-memory": {"serverUrl":"http://example.invalid/mcp"},
            "custom-memory": {"serverUrl":"http://lan:49374/mcp"},
            "other": {"serverUrl":"http://other/mcp"}
          }
        }"#,
    )
    .unwrap();

    let status = Command::new(bin())
        .args([
            "uninstall",
            "--apply",
            "--only",
            "mcp",
            "--mcp-url",
            "http://lan:49374/mcp",
            "--yes",
        ])
        .env("HOME", home.path())
        .env("AI_MEMORY_DATA_DIR", home.path().join(".ai-memory-data"))
        .status()
        .unwrap();
    assert!(status.success(), "uninstall failed");

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&mcp).unwrap()).unwrap();
    assert!(after["mcpServers"].get("custom-memory").is_none());
    assert!(
        after["mcpServers"].get("ai-memory").is_some(),
        "same name with a different endpoint must survive"
    );
    assert!(after["mcpServers"].get("other").is_some());
}

#[test]
fn uninstall_mcp_name_narrows_endpoint_match() {
    let _guard = cli_test_lock();
    let home = tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude.json");
    std::fs::write(
        &claude,
        r#"{
          "mcpServers": {
            "ai-memory": {"url":"http://127.0.0.1:49374/mcp"},
            "ai-memory-alt": {"url":"http://127.0.0.1:49374/mcp"}
          }
        }"#,
    )
    .unwrap();

    let status = Command::new(bin())
        .args([
            "uninstall",
            "--apply",
            "--only",
            "mcp",
            "--mcp-name",
            "ai-memory",
            "--yes",
        ])
        .env("HOME", home.path())
        .env("AI_MEMORY_DATA_DIR", home.path().join(".ai-memory-data"))
        .status()
        .unwrap();
    assert!(status.success(), "uninstall failed");

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude).unwrap()).unwrap();
    assert!(after["mcpServers"].get("ai-memory").is_none());
    assert!(after["mcpServers"].get("ai-memory-alt").is_some());
}

#[test]
fn uninstall_dry_run_changes_nothing() {
    let _guard = cli_test_lock();
    let home = tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let original = r#"{"hooks":{"Stop":[{"matcher":"","hooks":[{"type":"command","command":"AI_MEMORY_HOOK_URL=x /a/stop.sh"}]}]}}"#;
    std::fs::write(claude.join("settings.json"), original).unwrap();

    let status = Command::new(bin())
        .args(["uninstall", "--only", "hooks"]) // no --apply
        .env("HOME", home.path())
        .env("AI_MEMORY_DATA_DIR", home.path().join(".ai-memory-data"))
        .status()
        .unwrap();
    assert!(status.success());

    let after = std::fs::read_to_string(claude.join("settings.json")).unwrap();
    assert_eq!(after, original, "dry-run must not modify the file");
}

#[test]
fn uninstall_purge_data_apply_wipes() {
    let _guard = cli_test_lock();
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    for sub in ["wiki", "db", "raw"] {
        std::fs::create_dir_all(data.path().join(sub)).unwrap();
        std::fs::write(data.path().join(sub).join("f.txt"), b"x").unwrap();
    }
    std::fs::create_dir_all(data.path().join("logs")).unwrap();
    std::fs::write(data.path().join("logs/app.log"), b"l").unwrap();

    let out = Command::new(bin())
        .args(["uninstall", "--apply", "--yes", "--purge-data"])
        .env("HOME", home.path())
        .env("AI_MEMORY_DATA_DIR", data.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    for sub in ["wiki", "db", "raw"] {
        assert!(data.path().join(sub).is_dir(), "{sub} dir should remain");
        assert!(
            !data.path().join(sub).join("f.txt").exists(),
            "{sub} emptied"
        );
    }
    assert!(data.path().join("logs/app.log").exists(), "logs preserved");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("✓ purged"), "stdout was: {stdout}");
}

#[test]
fn uninstall_dry_run_previews_purge() {
    let _guard = cli_test_lock();
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    for sub in ["wiki", "db", "raw"] {
        std::fs::create_dir_all(data.path().join(sub)).unwrap();
        std::fs::write(data.path().join(sub).join("f.txt"), b"x").unwrap();
    }

    let out = Command::new(bin())
        .args(["uninstall", "--purge-data"]) // dry-run: no --apply
        .env("HOME", home.path())
        .env("AI_MEMORY_DATA_DIR", data.path())
        .output()
        .unwrap();
    assert!(out.status.success());

    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("would purge"), "stdout was: {stdout}");
    for sub in ["wiki", "db", "raw"] {
        let p = data.path().join(sub);
        assert!(
            stdout.contains(&p.display().to_string()),
            "missing {sub} in: {stdout}"
        );
        // Dry-run must not delete.
        assert!(p.join("f.txt").exists(), "{sub} must be untouched");
    }
}

/// Best-effort, NOT in the default run (sysinfo reads the real process table;
/// no injection seam). Spawns a real sibling `ai-memory` process and asserts
/// `--purge-data` refuses up front, leaving the wiring intact. Run with:
/// `cargo test -p ai-memory-cli --test uninstall -- --ignored`.
#[test]
#[ignore]
fn purge_data_refuses_when_sibling_alive() {
    let _guard = cli_test_lock();
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let settings = claude.join("settings.json");
    let original = r#"{"hooks":{"Stop":[{"matcher":"","hooks":[{"type":"command","command":"AI_MEMORY_HOOK_URL=x /a/stop.sh"}]}]}}"#;
    std::fs::write(&settings, original).unwrap();

    // Long-lived sibling `ai-memory` process.
    let mut serve = Command::new(bin())
        .arg("serve")
        .env("HOME", home.path())
        .env("AI_MEMORY_DATA_DIR", data.path())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(800));

    let out = Command::new(bin())
        .args(["uninstall", "--apply", "--yes", "--purge-data"])
        .env("HOME", home.path())
        .env("AI_MEMORY_DATA_DIR", data.path())
        .output()
        .unwrap();

    serve.kill().ok();
    serve.wait().ok();

    assert!(
        !out.status.success(),
        "should refuse while a sibling is alive"
    );
    // All-or-nothing: wiring must be untouched.
    assert_eq!(
        std::fs::read_to_string(&settings).unwrap(),
        original,
        "no wiring should be removed when the purge is refused up front"
    );
}
