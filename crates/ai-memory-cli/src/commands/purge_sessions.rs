//! `ai-memory purge-sessions` — delete all sessions in a project.

use anyhow::{Result, bail};
use serde::Serialize;

use crate::cli::PurgeSessionsArgs;
use crate::config::Config;
use crate::http_client::{ServerEndpoint, post_json};

/// Request sent to `POST /admin/purge-sessions`.
#[derive(Serialize)]
struct PurgeSessionsRequest {
    workspace: String,
    project: String,
    confirm: bool,
}

/// Run the `purge-sessions` subcommand.
///
/// Resolves the project name (auto-derived from the git repo root when
/// `--project` is omitted), requires `--confirm` before sending the
/// destructive request, then prints the JSON summary.
///
/// # Errors
/// Returns an error when `--confirm` is absent, the server is unreachable,
/// or the server returns a non-2xx response.
pub async fn run(config: &Config, args: PurgeSessionsArgs) -> Result<()> {
    let project = super::resolve_project_name(config, args.project.as_deref())?;

    if !args.confirm {
        bail!(
            "purge-sessions is destructive and irreversible.\n\
             Re-run with --confirm to proceed:\n\n  \
             ai-memory purge-sessions --workspace {} --project {} --confirm",
            args.workspace,
            project,
        );
    }

    let endpoint = ServerEndpoint::from_config_resolving_auth(config).await;
    let report: serde_json::Value = post_json(
        &endpoint,
        "/admin/purge-sessions",
        &PurgeSessionsRequest {
            workspace: args.workspace.clone(),
            project: project.clone(),
            confirm: true,
        },
    )
    .await?;

    let fallback_label = format!("{}/{}", args.workspace, project);
    let label = report["label"].as_str().unwrap_or(&fallback_label);
    let sessions = report["sessions_deleted"].as_u64().unwrap_or(0);
    let observations = report["observations_deleted"].as_u64().unwrap_or(0);
    let handoffs = report["handoffs_deleted"].as_u64().unwrap_or(0);
    let pages = report["pages_deleted"].as_u64().unwrap_or(0);
    let embeddings = report["embeddings_deleted"].as_u64().unwrap_or(0);
    println!(
        "Purged {}: {} sessions, {} observations, {} handoffs, {} pages, {} embeddings.",
        label, sessions, observations, handoffs, pages, embeddings
    );
    if let Some(failed) = report["files_failed"].as_array()
        && !failed.is_empty()
    {
        println!(
            "Warning: {} session summary page file(s) could not be removed from disk (DB rows are gone).",
            failed.len()
        );
    }
    if report["project_deleted"].as_bool().unwrap_or(false) {
        println!(
            "Reaped empty project {label} (no remaining sessions, observations, handoffs, or pages)."
        );
    }
    if report["workspace_deleted"].as_bool().unwrap_or(false) {
        println!(
            "Reaped empty workspace {ws} (no remaining projects).",
            ws = label.split('/').next().unwrap_or("")
        );
    }
    Ok(())
}
