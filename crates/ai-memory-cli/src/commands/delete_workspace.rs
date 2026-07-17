//! `ai-memory delete-workspace` — thin HTTP client for workspace deletion.

use anyhow::{Result, bail};

use crate::cli::DeleteWorkspaceArgs;
use crate::config::Config;
use crate::http_client::{ServerEndpoint, post_json};

/// Run the `delete-workspace` subcommand.
///
/// Requires `--confirm` before sending the destructive request. A non-empty
/// workspace also requires `--force`, since deleting it cascades to every
/// project, page, session, observation, and handoff it contains. Prints a
/// JSON summary of what was removed.
///
/// # Errors
/// Returns an error when `--confirm` is absent, the server is unreachable,
/// or the server returns a non-2xx response (e.g. 404 unknown workspace,
/// 409 workspace not empty without `--force`).
pub async fn run(config: &Config, args: DeleteWorkspaceArgs) -> Result<()> {
    if !args.confirm {
        bail!(
            "delete-workspace is destructive and irreversible.\n\
             Re-run with --confirm to proceed:\n\n  \
             ai-memory delete-workspace --workspace {} --confirm{}",
            args.workspace,
            if args.force { "" } else { " --force" },
        );
    }

    let endpoint = ServerEndpoint::from_config_resolving_auth(config).await;
    let report: serde_json::Value = post_json(
        &endpoint,
        "/admin/delete-workspace",
        &serde_json::json!({
            "workspace": args.workspace,
            "force": args.force,
        }),
    )
    .await?;

    let projects = report["projects_deleted"].as_u64().unwrap_or(0);
    let pages = report["pages_deleted"].as_u64().unwrap_or(0);
    println!(
        "Deleted workspace {}: {projects} projects, {pages} pages.",
        args.workspace
    );
    if let Some(failed) = report["files_failed"].as_array()
        && !failed.is_empty()
    {
        println!(
            "Warning: {} workspace file(s) could not be removed from disk (DB rows are gone).",
            failed.len()
        );
    }
    Ok(())
}
