//! axum router exposing `POST /hook`.
//!
//! Always returns 202 immediately. Heavy work (DB writes, session-page
//! synthesis) happens *after* the response is sent — but we still
//! `await` the writer ack to honour the cross-cutting invariant that
//! "indexes commit in the same transaction as the data" (no
//! background-task-indexing-after-return, basic-memory #763). The agent
//! never blocks on us thanks to the fire-and-forget client side.

use std::str::FromStr;
use std::sync::Arc;

use ai_memory_consolidate::Consolidator;
use ai_memory_core::{
    AgentKind, Handoff, NewHandoff, NewObservation, NewSession, ObservationKind, ProjectId,
    SessionId, WorkspaceId,
};
use ai_memory_store::WriterHandle;
use ai_memory_wiki::Wiki;
use axum::Json;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use jiff::Timestamp;
use serde::Deserialize;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::log;
use crate::payload::{HookEnvelope, HookEvent, HookQuery, parse_agent};
use crate::sanitize::Sanitized;
use crate::synth::synthesize_session_page;

/// Shared state passed to the hook handler.
#[derive(Clone)]
pub struct HookState {
    /// Workspace these observations belong to.
    pub workspace_id: WorkspaceId,
    /// Project these observations belong to.
    pub project_id: ProjectId,
    /// Writer actor handle.
    pub writer: WriterHandle,
    /// Reader pool — needed for session-end synthesis.
    pub reader: ai_memory_store::ReaderPool,
    /// Wiki handle — used to write the session-summary page.
    pub wiki: Wiki,
    /// Optional LLM-driven consolidator. When set, PreCompact uses it
    /// to refresh `sessions/<id>.md` before the agent loses its
    /// working context. When `None`, falls back to the deterministic
    /// rule-based synth (still useful, just lower-signal).
    pub consolidator: Option<Arc<Consolidator>>,
}

/// Build a router with `POST /hook` (event ingress) and `GET /handoff`
/// (synchronous handoff-fetch for session-start hooks).
pub fn hook_router(state: HookState) -> Router {
    Router::new()
        .route("/hook", post(handle_hook))
        .route("/handoff", get(handle_handoff))
        .with_state(Arc::new(state))
}

async fn handle_hook(
    State(state): State<Arc<HookState>>,
    Query(query): Query<HookQuery>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let env = HookEnvelope::from_query_and_body(query, body);
    tokio::spawn(process_envelope(state.clone(), env));
    (StatusCode::ACCEPTED, "queued")
}

/// Query params for `GET /handoff`.
#[derive(Debug, Clone, Deserialize)]
pub struct HandoffQuery {
    /// Identifier of the agent fetching the handoff. Used to mark the
    /// handoff as accepted-by; defaults to `Other` if unrecognised.
    pub agent: Option<String>,
    /// Optional cwd filter. When provided, only handoffs whose stored
    /// cwd matches this string are returned. Note: the cwd string is
    /// not canonicalized; symlinked paths must match byte-for-byte.
    pub cwd: Option<String>,
}

/// Synchronous endpoint used by `session-start.sh` to discover any
/// pending handoff from a previous agent. Returns plain text Markdown
/// (or an empty body when no handoff is open) with a 1-second cap on
/// the server side so the agent never blocks measurably on startup.
///
/// Side effect: when a handoff is found, it is *marked accepted* before
/// the response is sent. Two agents starting in parallel therefore
/// race; whichever arrives first wins. That is intentional — handoffs
/// are 1:1, not broadcast.
async fn handle_handoff(
    State(state): State<Arc<HookState>>,
    Query(query): Query<HandoffQuery>,
) -> impl IntoResponse {
    match fetch_and_accept_handoff(&state, query).await {
        Ok(Some(markdown)) => (StatusCode::OK, markdown),
        Ok(None) => (StatusCode::OK, String::new()),
        Err(e) => {
            warn!(error = %e, "handoff fetch failed");
            (StatusCode::OK, String::new())
        }
    }
}

async fn fetch_and_accept_handoff(
    state: &HookState,
    query: HandoffQuery,
) -> anyhow::Result<Option<String>> {
    let agent = query.agent.as_deref().map_or(AgentKind::Other, parse_agent);
    let handoff = state
        .reader
        .latest_open_handoff(state.workspace_id, state.project_id, query.cwd)
        .await?;
    let Some(h) = handoff else {
        return Ok(None);
    };
    state.writer.accept_handoff(h.id, agent, None).await?;
    Ok(Some(render_handoff_markdown(&h)))
}

fn render_handoff_markdown(h: &Handoff) -> String {
    let mut buf = String::with_capacity(512);
    buf.push_str("> ai-memory — pending handoff from previous session\n");
    buf.push_str(&format!(
        "> from: {from} | created: {ts}\n",
        from = h.from_agent.as_str(),
        ts = h.created_at,
    ));
    buf.push_str("\n## Summary\n");
    buf.push_str(&h.summary);
    if !h.open_questions.is_empty() {
        buf.push_str("\n\n## Open questions\n");
        for q in &h.open_questions {
            buf.push_str(&format!("- {q}\n"));
        }
    }
    if !h.next_steps.is_empty() {
        buf.push_str("\n## Next steps\n");
        for s in &h.next_steps {
            buf.push_str(&format!("- {s}\n"));
        }
    }
    if !h.files_touched.is_empty() {
        buf.push_str("\n## Files touched recently\n");
        for f in &h.files_touched {
            buf.push_str(&format!("- `{f}`\n"));
        }
    }
    buf.push_str(
        "\n_Tip: call `memory_query` or `memory_recent` to recover \
         more detail from prior sessions._\n",
    );
    buf
}

async fn process_envelope(state: Arc<HookState>, env: HookEnvelope) {
    if let Err(e) = process(&state, env).await {
        warn!(error = %e, "hook processing failed");
    }
}

async fn process(state: &HookState, env: HookEnvelope) -> anyhow::Result<()> {
    let session_id = resolve_session_id(&env)?;

    // Begin the session row if SessionStart, otherwise no-op (the
    // `INSERT OR IGNORE` makes this safe).
    if matches!(env.event, HookEvent::SessionStart) {
        let new_session = NewSession {
            id: session_id,
            workspace_id: state.workspace_id,
            project_id: state.project_id,
            agent_kind: env.agent,
            cwd: env.cwd.as_ref().map(std::path::PathBuf::from),
        };
        state.writer.begin_session(new_session).await?;
    }

    // Persist the observation row.
    let kind = env.event.to_observation_kind();
    let title = env
        .title_hint
        .clone()
        .unwrap_or_else(|| kind.as_str().to_string());
    let body = env.body_excerpt.clone().unwrap_or_default();
    let raw_obs = NewObservation {
        session_id,
        workspace_id: state.workspace_id,
        project_id: state.project_id,
        kind,
        title,
        body,
        importance: importance_for(env.event),
    };
    let sanitized = Sanitized::new(raw_obs);
    let _ = state
        .writer
        .insert_observation(sanitized.inner().clone())
        .await?;

    // Append the log line.
    if let Err(e) = log::append_event(
        state.wiki.root(),
        Timestamp::now(),
        env.event,
        sanitized.inner().title.as_str(),
    ) {
        warn!(error = %e, "log.md append failed");
    }

    // On PreCompact, refresh `sessions/<id>.md` so the wiki captures
    // the working state before the agent's compaction throws it out
    // of context. Does NOT end the session and does NOT create a
    // handoff. The eventual SessionEnd supersedes this page.
    if matches!(env.event, HookEvent::PreCompact) {
        if let Err(e) = consolidate_or_synth(state, session_id).await {
            warn!(error = %e, "PreCompact consolidation failed; continuing");
        }
    }

    // On SessionEnd, synthesize the summary page, end the session, and
    // auto-create a handoff so the next agent can pick up.
    if matches!(env.event, HookEvent::SessionEnd) {
        let observations = state.reader.observations_for_session(session_id).await?;
        let new_page = synthesize_session_page(
            state.workspace_id,
            state.project_id,
            session_id,
            &observations,
        );
        let page_id = state
            .wiki
            .write_page(ai_memory_wiki::WritePageRequest {
                workspace_id: new_page.workspace_id,
                project_id: new_page.project_id,
                path: new_page.path.clone(),
                frontmatter: new_page.frontmatter_json.clone(),
                body: new_page.body.clone(),
                tier: new_page.tier,
                pinned: new_page.pinned,
                title: None,
            })
            .await?;
        state.writer.end_session(session_id, Some(page_id)).await?;
        let handoff =
            build_auto_handoff(state, env.agent, session_id, env.cwd.clone(), &observations);
        let handoff_id = state.writer.insert_handoff(handoff).await?;
        // Auto-commit the wiki tree so the session/handoff/log.md
        // changes land in git in one atomic snapshot.
        let commit_msg = format!(
            "session {}: {}",
            short_id(&session_id.to_string()),
            new_page.title.chars().take(60).collect::<String>(),
        );
        match state.wiki.commit_all(&commit_msg) {
            Ok(Some(oid)) => debug!(commit = %oid, "wiki auto-commit"),
            Ok(None) => debug!("wiki clean; no auto-commit"),
            Err(e) => warn!(error = %e, "auto-commit failed"),
        }
        info!(
            session = %session_id,
            page = %new_page.path,
            handoff = %handoff_id,
            "session ended; summary page + open handoff created",
        );
    }

    Ok(())
}

fn resolve_session_id(env: &HookEnvelope) -> anyhow::Result<SessionId> {
    if let Some(raw) = &env.session_id {
        // Accept either a UUID (canonical) or any string, hashing the
        // latter to a deterministic UUID v5 so each agent's session id
        // maps cleanly into our schema.
        if let Ok(id) = SessionId::from_str(raw) {
            return Ok(id);
        }
        let uuid = Uuid::new_v5(&Uuid::NAMESPACE_OID, raw.as_bytes());
        return Ok(SessionId(uuid));
    }
    if matches!(env.event, HookEvent::SessionStart) {
        return Ok(SessionId::new());
    }
    anyhow::bail!("hook payload missing session_id and event is not session-start")
}

fn build_auto_handoff(
    state: &HookState,
    from_agent: AgentKind,
    session_id: SessionId,
    cwd: Option<String>,
    observations: &[ai_memory_core::Observation],
) -> NewHandoff {
    let mut prompts: Vec<&str> = Vec::new();
    let mut tools: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for obs in observations {
        match obs.kind {
            ObservationKind::UserPrompt if !obs.title.is_empty() => prompts.push(&obs.title),
            ObservationKind::PostToolUse | ObservationKind::PreToolUse if !obs.title.is_empty() => {
                tools.insert(&obs.title);
            }
            _ => {}
        }
    }
    let last_prompt = prompts.last().copied();
    let summary = match (prompts.first().copied(), last_prompt) {
        (Some(first), Some(last)) if first == last => format!("Session focused on: {first}"),
        (Some(first), Some(last)) => format!("Started: {first}. Last: {last}."),
        (Some(first), None) => format!("Started: {first}."),
        _ => format!(
            "Session ended; {} observations recorded.",
            observations.len()
        ),
    };
    let open_questions = if prompts.is_empty() {
        Vec::new()
    } else {
        // Heuristic: last user prompt often *is* the open question.
        vec![format!(
            "Continue from: {}",
            last_prompt.unwrap_or_default()
        )]
    };
    let next_steps = if tools.is_empty() {
        Vec::new()
    } else {
        vec![format!(
            "Tools used: {}",
            tools.into_iter().collect::<Vec<_>>().join(", ")
        )]
    };
    NewHandoff {
        workspace_id: state.workspace_id,
        project_id: state.project_id,
        from_session_id: Some(session_id),
        from_agent,
        to_agent: None,
        cwd: cwd.map(std::path::PathBuf::from),
        summary,
        open_questions,
        next_steps,
        files_touched: Vec::new(),
    }
}

/// Write a fresh `sessions/<id>.md` for the current session without
/// ending it. Used by the PreCompact branch to checkpoint state before
/// the agent's working context collapses.
async fn consolidate_or_synth(state: &HookState, session_id: SessionId) -> anyhow::Result<()> {
    if let Some(c) = state.consolidator.as_ref() {
        let outcome = c.consolidate_session(session_id, false).await?;
        debug!(
            session = %session_id,
            path = %outcome.path,
            "PreCompact: LLM consolidation written",
        );
        let _ = state.wiki.commit_all(&format!(
            "pre-compact(session {}): checkpoint",
            short_id(&session_id.to_string()),
        ));
        return Ok(());
    }
    let observations = state.reader.observations_for_session(session_id).await?;
    if observations.is_empty() {
        return Ok(());
    }
    let new_page = synthesize_session_page(
        state.workspace_id,
        state.project_id,
        session_id,
        &observations,
    );
    state
        .wiki
        .write_page(ai_memory_wiki::WritePageRequest {
            workspace_id: new_page.workspace_id,
            project_id: new_page.project_id,
            path: new_page.path,
            frontmatter: new_page.frontmatter_json,
            body: new_page.body,
            tier: new_page.tier,
            pinned: new_page.pinned,
            title: None,
        })
        .await?;
    let _ = state.wiki.commit_all(&format!(
        "pre-compact(session {}): checkpoint",
        short_id(&session_id.to_string()),
    ));
    debug!(session = %session_id, "PreCompact: rule-based checkpoint written");
    Ok(())
}

fn short_id(s: &str) -> String {
    s.chars().take(8).collect()
}

const fn importance_for(event: HookEvent) -> u8 {
    match event {
        HookEvent::SessionStart | HookEvent::SessionEnd => 7,
        HookEvent::UserPrompt => 8,
        HookEvent::PostToolUse | HookEvent::PreToolUse => 5,
        HookEvent::Stop | HookEvent::PreCompact => 6,
        HookEvent::Notification | HookEvent::Other => 3,
    }
}

#[allow(dead_code)]
fn _agent_kind_marker(_k: AgentKind) {}
