//! `ai-memory rename-workspace` — thin HTTP client for workspace rename.

use anyhow::Result;

use crate::cli::RenameWorkspaceArgs;
use crate::config::Config;
use crate::http_client::{ServerEndpoint, post_json};

/// Run the `rename-workspace` subcommand.
///
/// Sends the rename request to the server, then prints a human-readable
/// confirmation line. Workspaces are UUID-keyed on disk, so this is a
/// column-only rename — nothing moves on disk.
///
/// # Errors
/// Returns an error when the server is unreachable or returns a non-2xx
/// response (e.g. 404 unknown workspace, 422 name taken or invalid).
pub async fn run(config: &Config, args: RenameWorkspaceArgs) -> Result<()> {
    let endpoint = ServerEndpoint::from_config_resolving_auth(config).await;
    let body = serde_json::json!({
        "from": args.from,
        "to": args.to,
    });
    let summary: serde_json::Value = post_json(&endpoint, "/admin/rename-workspace", &body).await?;
    let from = summary["from"].as_str().unwrap_or(&args.from);
    let to = summary["to"].as_str().unwrap_or(&args.to);
    let manifests = summary["manifests_refreshed"].as_u64().unwrap_or(0);
    println!("Renamed workspace {from} → {to} ({manifests} scope manifest(s) refreshed).");
    if let Some(warning) = summary["manifest_warning"].as_str() {
        println!("Warning: {warning}");
    }
    Ok(())
}
