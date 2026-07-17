//! `ai-memory purge-session` — thin HTTP client for session purge.

use std::str::FromStr;

use anyhow::{Result, bail};
use serde::Serialize;

use crate::cli::PurgeSessionArgs;
use crate::config::Config;
use crate::http_client::{ServerEndpoint, post_json};

/// Request sent to `POST /admin/purge-session`.
#[derive(Serialize)]
struct PurgeSessionRequest {
    id: String,
    confirm: bool,
}

/// Run the `purge-session` subcommand.
///
/// Requires `--confirm` before sending the destructive request, then prints
/// the JSON summary returned by the server.
///
/// # Errors
/// Returns an error when `--confirm` is absent, the session id is malformed,
/// the server is unreachable, or the server returns a non-2xx response.
pub async fn run(config: &Config, args: PurgeSessionArgs) -> Result<()> {
    // Validate the id up front so a typo fails locally with a clear message
    // rather than a 400 from the server.
    if ai_memory_core::SessionId::from_str(&args.id).is_err() {
        bail!(
            "purge-session: `--id` must be a valid session UUID: {}",
            args.id
        );
    }

    let endpoint = ServerEndpoint::from_config_resolving_auth(config).await;
    let report: serde_json::Value = post_json(
        &endpoint,
        "/admin/purge-session",
        &PurgeSessionRequest {
            id: args.id.clone(),
            confirm: true,
        },
    )
    .await?;

    let session_id = report["session_id"].as_str().unwrap_or(&args.id);
    let label = report["label"].as_str().unwrap_or("");
    let observations = report["observations_deleted"].as_u64().unwrap_or(0);
    let handoffs = report["handoffs_deleted"].as_u64().unwrap_or(0);
    let pages = report["pages_deleted"].as_u64().unwrap_or(0);
    let embeddings = report["embeddings_deleted"].as_u64().unwrap_or(0);
    let file_deleted = report["file_deleted"].as_bool().unwrap_or(false);
    println!(
        "Purged session {session_id} ({label}): {observations} observations, \
         {handoffs} handoffs, {pages} pages, {embeddings} embeddings \
         (summary file removed: {file_deleted})."
    );
    if let Some(failed) = report["file_failed"].as_str() {
        println!(
            "Warning: summary page file could not be removed from disk: {failed} \
             (DB rows are gone)."
        );
    }
    Ok(())
}
