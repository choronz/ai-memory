//! `ai-memory move-project` — thin HTTP client for cross-workspace project move.

use anyhow::{Result, anyhow, bail};
use serde::Serialize;

use crate::cli::MoveProjectArgs;
use crate::config::Config;
use crate::http_client::{ServerEndpoint, post_json};

/// Request sent to `POST /admin/move-project`.
#[derive(Serialize)]
struct MoveProjectRequest {
    from_workspace: String,
    project: String,
    to_workspace: String,
    confirm: bool,
    force: bool,
    on_conflict: String,
}

/// Run the `move-project` subcommand.
///
/// Resolves the source project name (auto-derived from the git repo root
/// when `--project` is omitted) and the source/destination workspaces
/// (auto-derived from the repo's `.ai-memory.toml` marker and CWD when the
/// `--from-workspace` / `--to-workspace` flags are omitted). Requires
/// `--confirm` before sending the request (a true-move re-stamp or a
/// copy+purge merge, both irreversible), then prints the report.
///
/// # Errors
/// Returns an error when `--confirm` is absent, the destination workspace is
/// unspecified, the server is unreachable, or the server returns a non-2xx
/// response.
pub async fn run(config: &Config, args: MoveProjectArgs) -> Result<()> {
    let project = super::resolve_project_name(config, args.project.as_deref())?;
    // Send an empty `from_workspace` when the user didn't name one, so the
    // server resolves the source by a cross-workspace lookup on the project
    // name (the same global fallback `purge-project` uses) — the project may
    // live in a workspace other than the caller's CWD marker.
    let from_workspace = args.from_workspace.clone().unwrap_or_default();
    // When the source workspace wasn't named, the server resolves it by a
    // cross-workspace lookup on the project name; show that in the preview.
    let from_label = if from_workspace.is_empty() {
        "(auto)".to_string()
    } else {
        from_workspace.clone()
    };
    let to_workspace = args
        .new_workspace
        .clone()
        .or_else(|| args.to_workspace.clone())
        .ok_or_else(|| {
            anyhow!(
                "a destination workspace is required: pass --workspace <new> or --to-workspace <new>"
            )
        })?;

    if !args.confirm {
        bail!(
            "move-project moves {}/{} to workspace {}. If the destination has \
             no same-named project it is a lossless true-move (re-stamp in \
             place — sessions, observations and history preserved). If it \
             already has one, the pages are copied in and merged, then the \
             source is PURGED. Both are irreversible.\n\
             Re-run with --confirm to proceed:\n\n  \
             ai-memory move-project --from-workspace {} --project {} \
             --to-workspace {} --confirm",
            from_label,
            project,
            to_workspace,
            from_workspace,
            project,
            to_workspace,
        );
    }

    let endpoint = ServerEndpoint::from_config_resolving_auth(config).await;
    let report: serde_json::Value = post_json(
        &endpoint,
        "/admin/move-project",
        &MoveProjectRequest {
            from_workspace: from_workspace.clone(),
            project: project.clone(),
            to_workspace: to_workspace.clone(),
            confirm: true,
            force: args.force,
            on_conflict: args.on_conflict.clone(),
        },
    )
    .await?;

    let pages = report["pages_copied"].as_u64().unwrap_or(0);
    let purged = report["source_purged"].as_bool().unwrap_or(false);
    let moved_via = report["moved_via"].as_str().unwrap_or("");
    let skipped_count = report["pages_skipped"].as_array().map_or(0, |s| s.len());
    // The server reports the resolved source/destination labels (the source
    // workspace may have been resolved by a cross-workspace lookup), so prefer
    // those for the human summary.
    let from_label = report["from"].as_str().unwrap_or(&from_label).to_string();
    let to_label = report["to"].as_str().unwrap_or(&to_workspace).to_string();

    if moved_via == "true-move" {
        // Lossless: re-stamped in place, nothing copied or purged.
        println!(
            "Moved {from_label} → {to_label}: {pages} pages re-stamped (true move — \
             sessions, observations and history preserved).",
        );
    } else {
        // copy-purge (merge into an existing same-named project).
        let tail = if skipped_count > 0 {
            ", SOURCE LEFT INTACT (some pages unreadable — fix and re-run)"
        } else if purged {
            ", source purged"
        } else {
            ", SOURCE LEFT INTACT (partial copy)"
        };
        println!(
            "Moved {from_label} → {to_label}: {pages} pages copied (merged into existing \
             project){tail}.",
        );
        if skipped_count > 0 {
            println!(
                "Warning: {skipped_count} page(s) could not be read from the \
                 source and were skipped; the source was NOT purged. Fix and re-run.",
            );
        }
        if let Some(conflicts) = report["conflicts"].as_array().filter(|c| !c.is_empty()) {
            println!(
                "{} path conflict(s) — source page kept under a de-duplicated path \
                 (both versions preserved):",
                conflicts.len()
            );
            for c in conflicts {
                println!(
                    "  {} → {}",
                    c["path"].as_str().unwrap_or("?"),
                    c["moved_to"].as_str().unwrap_or("?"),
                );
            }
        }
    }
    Ok(())
}
