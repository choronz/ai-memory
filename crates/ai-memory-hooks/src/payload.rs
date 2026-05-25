//! Wire envelope received on `POST /hook`.

use ai_memory_core::{AgentKind, ObservationKind};
use serde::{Deserialize, Serialize};

/// Query-string parameters on `POST /hook`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct HookQuery {
    /// Lifecycle event identifier (kebab-case or snake_case).
    pub event: String,
    /// Agent CLI identifier (`claude-code`, `codex`, `cursor`, etc.).
    pub agent: Option<String>,
    /// Working directory of the agent at the time the hook fired.
    /// Most agents put this in the JSON body, but accepting it on the
    /// query string too lets `curl` / tests / non-Claude bridges
    /// populate it without constructing a body envelope.
    pub cwd: Option<String>,
    /// Workspace name override (typically declared by the agent's
    /// host-side hook via a `.ai-memory.toml` walk-up). When `None`
    /// the server falls back to `DEFAULT_WORKSPACE_NAME`.
    pub workspace: Option<String>,
    /// Project name override (same source as `workspace`). When
    /// `None` the server falls back to `basename(cwd)`.
    pub project: Option<String>,
    /// Optional third-party extension namespace. When present, ai-memory
    /// preserves a validated source event name without expanding the
    /// closed core event vocabulary.
    pub extension: Option<String>,
    /// Optional explicit source event name for extension vocabularies.
    /// When omitted and `extension` is present, unknown `event` values
    /// are preserved as the source event.
    pub source_event: Option<String>,
}

/// Coalesced view of an incoming hook event after light parsing of the
/// body. We keep the original raw JSON around so consumers can extract
/// agent-specific fields they care about.
#[derive(Debug, Clone, Serialize)]
pub struct HookEnvelope {
    /// Mapped lifecycle event.
    pub event: HookEvent,
    /// Agent CLI identifier.
    pub agent: AgentKind,
    /// Session identifier, if found in the body. Required for everything
    /// except the initial `SessionStart`.
    pub session_id: Option<String>,
    /// Current working directory at the time of the event.
    pub cwd: Option<String>,
    /// Workspace name override declared by the hook (via marker file
    /// walk-up). Empty / `None` defers to `DEFAULT_WORKSPACE_NAME`.
    pub workspace_override: Option<String>,
    /// Project name override declared by the hook. Empty / `None`
    /// defers to `basename(cwd)`.
    pub project_override: Option<String>,
    /// Optional third-party extension namespace.
    pub extension: Option<String>,
    /// Optional source event name from the extension vocabulary.
    pub source_event: Option<String>,
    /// Optional title hint extracted from the body.
    pub title_hint: Option<String>,
    /// Optional body excerpt extracted from the agent's raw payload.
    pub body_excerpt: Option<String>,
    /// The agent's raw JSON, kept for forensics.
    pub raw: serde_json::Value,
}

/// Discriminator for the lifecycle event that triggered the hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HookEvent {
    /// New session started (capture cwd + model).
    SessionStart,
    /// User submitted a prompt.
    UserPrompt,
    /// Agent is about to call a tool.
    PreToolUse,
    /// Agent finished a tool call.
    PostToolUse,
    /// Compaction event (context window pressure).
    PreCompact,
    /// Agent emitted a notification.
    Notification,
    /// Agent finished its turn (interactive `/stop` or natural end).
    Stop,
    /// Session ended (final).
    SessionEnd,
    /// Anything else.
    Other,
}

impl HookEvent {
    /// Parse a kebab- or snake-case event identifier into [`HookEvent`].
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s {
            "session-start" | "session_start" | "SessionStart" | "sessionStart" => {
                Self::SessionStart
            }
            "user-prompt" | "user_prompt" | "UserPromptSubmit" | "beforeSubmitPrompt" => {
                Self::UserPrompt
            }
            "pre-tool-use" | "pre_tool_use" | "PreToolUse" | "preToolUse" | "BeforeTool" => {
                Self::PreToolUse
            }
            "post-tool-use" | "post_tool_use" | "PostToolUse" | "postToolUse"
            | "postToolUseFailure" | "PostToolUseFailure" | "AfterTool" => Self::PostToolUse,
            "pre-compact" | "pre_compact" | "PreCompact" | "preCompact" | "PreCompress" => {
                Self::PreCompact
            }
            "notification" | "Notification" => Self::Notification,
            "stop" | "Stop" => Self::Stop,
            "session-end" | "session_end" | "SessionEnd" | "sessionEnd" => Self::SessionEnd,
            _ => Self::Other,
        }
    }

    /// Map to the storage-level [`ObservationKind`].
    #[must_use]
    pub const fn to_observation_kind(self) -> ObservationKind {
        match self {
            Self::SessionStart => ObservationKind::SessionStart,
            Self::UserPrompt => ObservationKind::UserPrompt,
            Self::PreToolUse => ObservationKind::PreToolUse,
            Self::PostToolUse => ObservationKind::PostToolUse,
            Self::PreCompact => ObservationKind::PreCompact,
            Self::Notification => ObservationKind::Notification,
            Self::Stop => ObservationKind::Stop,
            Self::SessionEnd => ObservationKind::SessionEnd,
            Self::Other => ObservationKind::Other,
        }
    }
}

/// Parse an agent identifier into [`AgentKind`]. Unknown values map to
/// [`AgentKind::Other`].
#[must_use]
pub fn parse_agent(s: &str) -> AgentKind {
    AgentKind::from_wire(s)
}

impl HookEnvelope {
    /// Build an envelope from the parsed query + the body JSON. Performs
    /// best-effort extraction of `session_id` / `cwd` / a body excerpt
    /// from common shapes used by Claude Code, Codex, and OpenCode hook
    /// payloads.
    #[must_use]
    pub fn from_query_and_body(query: HookQuery, raw: serde_json::Value) -> Self {
        let event = HookEvent::parse(&query.event);
        let agent = query.agent.as_deref().map_or(AgentKind::Other, parse_agent);
        // OpenCode's plugin SDK sends `sessionID` (capital `ID`) on the
        // tool.execute.*/session.* events; Claude Code uses `session_id`,
        // Codex `sessionId`. JSON keys are case-sensitive, so all three
        // spellings must be listed or OpenCode tool events fail the
        // "missing session_id" check in the router (issue #1).
        let session_id = extract_string(&raw, &["session_id", "sessionId", "sessionID", "session"])
            .or_else(|| {
                extract_string_path(
                    &raw,
                    &[
                        &["info", "id"],
                        &["properties", "sessionID"],
                        &["properties", "info", "id"],
                        &["event", "properties", "sessionID"],
                        &["event", "properties", "info", "id"],
                        &["payload", "info", "id"],
                        &["payload", "properties", "sessionID"],
                        &["payload", "properties", "info", "id"],
                    ],
                )
            });
        let body_cwd = extract_string(&raw, &["cwd", "current_dir", "working_dir", "directory"])
            .or_else(|| {
                extract_string_path(
                    &raw,
                    &[
                        &["path", "cwd"],
                        &["info", "directory"],
                        &["properties", "info", "directory"],
                        &["event", "properties", "info", "directory"],
                        &["payload", "path", "cwd"],
                        &["payload", "info", "directory"],
                        &["payload", "properties", "info", "directory"],
                    ],
                )
            });
        // Body cwd wins over the query-string fallback: the body is
        // what agent CLIs natively send, so any query-string `cwd` is
        // a bridge / test override that should defer to live data.
        let cwd = body_cwd.or_else(|| query.cwd.filter(|s| !s.is_empty()));
        let workspace_override = query.workspace.filter(|s| !s.is_empty());
        let project_override = query.project.filter(|s| !s.is_empty());
        let extension = normalize_extension_name(query.extension.as_deref());
        let source_event = extension.as_ref().and_then(|_| {
            let raw_source = query
                .source_event
                .as_deref()
                .or_else(|| (event == HookEvent::Other).then_some(query.event.as_str()))?;
            normalize_source_event(raw_source)
        });
        let extension = if source_event.is_some() {
            extension
        } else {
            None
        };
        let title_hint = best_title_hint(event, &raw).or_else(|| {
            source_event
                .as_deref()
                .map(|source| extension_title_hint(&raw, source))
        });
        let body_excerpt = best_body_excerpt(event, &raw).or_else(|| {
            source_event
                .as_deref()
                .and_then(|_| extension_body_excerpt(&raw))
        });
        Self {
            event,
            agent,
            session_id,
            cwd,
            workspace_override,
            project_override,
            extension,
            source_event,
            title_hint,
            body_excerpt,
            raw,
        }
    }
}

fn extract_string(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        for candidate in extraction_candidates(value) {
            if let Some(s) = candidate.get(*key).and_then(serde_json::Value::as_str)
                && !s.is_empty()
            {
                return Some(s.to_string());
            }
        }
    }
    None
}

fn extract_string_path(value: &serde_json::Value, paths: &[&[&str]]) -> Option<String> {
    for path in paths {
        if let Some(s) = value_at_path(value, path).and_then(serde_json::Value::as_str)
            && !s.is_empty()
        {
            return Some(s.to_string());
        }
    }
    None
}

fn value_at_path<'a>(
    mut value: &'a serde_json::Value,
    path: &[&str],
) -> Option<&'a serde_json::Value> {
    for segment in path {
        value = value.get(*segment)?;
    }
    Some(value)
}

fn extraction_candidates(value: &serde_json::Value) -> Vec<&serde_json::Value> {
    let mut out = Vec::new();
    push_candidates(&mut out, value);
    if let Some(payload) = value.get("payload") {
        push_candidates(&mut out, payload);
    }
    if let Some(event) = value.get("event") {
        push_candidates(&mut out, event);
    }
    out
}

fn push_candidates<'a>(out: &mut Vec<&'a serde_json::Value>, value: &'a serde_json::Value) {
    out.push(value);
    if let Some(properties) = value.get("properties") {
        out.push(properties);
        if let Some(info) = properties.get("info") {
            out.push(info);
        }
    }
    if let Some(info) = value.get("info") {
        out.push(info);
    }
    if let Some(path) = value.get("path") {
        out.push(path);
    }
}

fn best_title_hint(event: HookEvent, raw: &serde_json::Value) -> Option<String> {
    match event {
        HookEvent::SessionStart => extract_string(raw, &["model", "title"]),
        HookEvent::UserPrompt => {
            extract_string(raw, &["prompt", "message", "text"]).map(|s| truncate_for_title(&s))
        }
        HookEvent::PreToolUse | HookEvent::PostToolUse => {
            extract_string(raw, &["tool", "tool_name", "name"])
        }
        HookEvent::Notification => extract_string(raw, &["message", "text"]),
        _ => None,
    }
}

fn extension_title_hint(raw: &serde_json::Value, source_event: &str) -> String {
    extract_string(raw, &["title", "summary", "subject", "name"])
        .map(|s| truncate_for_title(&s))
        .unwrap_or_else(|| source_event.to_string())
}

fn extension_body_excerpt(raw: &serde_json::Value) -> Option<String> {
    extract_string(
        raw,
        &[
            "body",
            "message",
            "text",
            "description",
            "summary",
            "details",
        ],
    )
    .map(|s| truncate_excerpt(&s))
}

fn best_body_excerpt(event: HookEvent, raw: &serde_json::Value) -> Option<String> {
    match event {
        HookEvent::UserPrompt => extract_string(raw, &["prompt", "message", "text"]),
        HookEvent::PostToolUse => {
            let tool = extract_string(raw, &["tool", "tool_name", "name"])?;
            let result = extract_string(raw, &["tool_response", "tool_output", "output", "result"])
                .unwrap_or_else(|| "(no output captured)".into());
            Some(format!("tool: {tool}\n---\n{}", truncate_excerpt(&result)))
        }
        HookEvent::Notification => extract_string(raw, &["message", "text"]),
        _ => None,
    }
}

fn truncate_for_title(s: &str) -> String {
    const MAX: usize = 80;
    let one_line: String = s.chars().take_while(|c| *c != '\n').collect();
    if one_line.chars().count() <= MAX {
        one_line
    } else {
        let mut buf: String = one_line.chars().take(MAX - 1).collect();
        buf.push('…');
        buf
    }
}

fn truncate_excerpt(s: &str) -> String {
    const MAX: usize = 2_000;
    if s.len() <= MAX {
        s.to_string()
    } else {
        let mut buf = String::with_capacity(MAX + '…'.len_utf8());
        let mut end = 0;
        for (idx, ch) in s.char_indices() {
            let next = idx + ch.len_utf8();
            if next > MAX {
                break;
            }
            end = next;
        }
        buf.push_str(&s[..end]);
        buf.push('…');
        buf
    }
}

fn normalize_extension_name(value: Option<&str>) -> Option<String> {
    normalize_token(value?, 64)
}

fn normalize_source_event(value: &str) -> Option<String> {
    normalize_token(value, 128)
}

fn normalize_token(value: &str, max_len: usize) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > max_len {
        return None;
    }
    if trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':'))
    {
        Some(trimmed.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_events() {
        assert_eq!(HookEvent::parse("session-start"), HookEvent::SessionStart);
        assert_eq!(HookEvent::parse("PreToolUse"), HookEvent::PreToolUse);
        assert_eq!(HookEvent::parse("user_prompt"), HookEvent::UserPrompt);
        assert_eq!(HookEvent::parse("bogus"), HookEvent::Other);
    }

    #[test]
    fn extension_event_preserves_source_event_when_opted_in() {
        let env = HookEnvelope::from_query_and_body(
            HookQuery {
                event: "lead.contact".into(),
                extension: Some("fstech".into()),
                ..Default::default()
            },
            serde_json::json!({
                "session_id": "fst-1",
                "title": "Lead contacted",
                "message": "Lead Maria requested a proposal"
            }),
        );

        assert_eq!(env.event, HookEvent::Other);
        assert_eq!(env.extension.as_deref(), Some("fstech"));
        assert_eq!(env.source_event.as_deref(), Some("lead.contact"));
        assert_eq!(env.title_hint.as_deref(), Some("Lead contacted"));
        assert_eq!(
            env.body_excerpt.as_deref(),
            Some("Lead Maria requested a proposal")
        );
    }

    #[test]
    fn unknown_event_without_extension_leaves_no_source_event() {
        let env = HookEnvelope::from_query_and_body(
            HookQuery {
                event: "lead.contact".into(),
                ..Default::default()
            },
            serde_json::json!({
                "session_id": "fst-1",
                "title": "Lead contacted",
                "message": "Lead Maria requested a proposal"
            }),
        );

        assert_eq!(env.event, HookEvent::Other);
        assert_eq!(env.extension, None);
        assert_eq!(env.source_event, None);
        assert_eq!(env.title_hint, None);
        assert_eq!(env.body_excerpt, None);
    }

    #[test]
    fn invalid_extension_tokens_are_not_preserved() {
        let env = HookEnvelope::from_query_and_body(
            HookQuery {
                event: "lead.contact".into(),
                extension: Some("bad extension".into()),
                ..Default::default()
            },
            serde_json::json!({ "session_id": "fst-1" }),
        );

        assert_eq!(env.extension, None);
        assert_eq!(env.source_event, None);
    }

    #[test]
    fn maps_to_observation_kind() {
        assert_eq!(
            HookEvent::SessionEnd.to_observation_kind(),
            ObservationKind::SessionEnd
        );
    }

    #[test]
    fn envelope_extracts_session_and_cwd() {
        let q = HookQuery {
            event: "session-start".into(),
            agent: Some("claude-code".into()),
            ..Default::default()
        };
        let raw = serde_json::json!({
            "session_id": "abc-123",
            "cwd": "/tmp/x",
            "model": "claude-sonnet-4-6"
        });
        let env = HookEnvelope::from_query_and_body(q, raw);
        assert_eq!(env.event, HookEvent::SessionStart);
        assert_eq!(env.session_id.as_deref(), Some("abc-123"));
        assert_eq!(env.cwd.as_deref(), Some("/tmp/x"));
        assert_eq!(env.title_hint.as_deref(), Some("claude-sonnet-4-6"));
    }

    /// OpenCode's plugin SDK sends `sessionID` (capital `ID`) on the
    /// tool.execute.* / session.* events. Regression for issue #1: this
    /// spelling must be extracted, otherwise non-session-start events
    /// fail the router's "missing session_id" check.
    #[test]
    fn envelope_extracts_opencode_camelcase_session_id() {
        let q = HookQuery {
            event: "post-tool-use".into(),
            agent: Some("open-code".into()),
            ..Default::default()
        };
        let raw = serde_json::json!({
            "sessionID": "ses_abc123",
            "tool": "bash",
            "callID": "call_1"
        });
        let env = HookEnvelope::from_query_and_body(q, raw);
        assert_eq!(env.session_id.as_deref(), Some("ses_abc123"));
    }

    /// Earlier OpenCode plugin generation wrapped the actual SDK hook
    /// input under `payload`. Keep accepting that shape so users with
    /// an old plugin don't silently lose project routing until they
    /// restart with the fixed plugin.
    #[test]
    fn envelope_extracts_legacy_opencode_nested_payload() {
        let q = HookQuery {
            event: "post-tool-use".into(),
            agent: Some("open-code".into()),
            ..Default::default()
        };
        let raw = serde_json::json!({
            "hook_event_name": "post-tool-use",
            "agent": "open-code",
            "payload": {
                "sessionID": "ses_nested",
                "cwd": "/home/user/ai-memory",
                "tool": "bash",
                "output": "tests passed"
            }
        });
        let env = HookEnvelope::from_query_and_body(q, raw);
        assert_eq!(env.session_id.as_deref(), Some("ses_nested"));
        assert_eq!(env.cwd.as_deref(), Some("/home/user/ai-memory"));
        assert_eq!(env.title_hint.as_deref(), Some("bash"));
        assert!(
            env.body_excerpt
                .as_deref()
                .is_some_and(|body| body.contains("tests passed")),
            "post-tool body should include nested output: {:?}",
            env.body_excerpt
        );
    }

    /// OpenCode's plugin `event` hook receives bus events shaped like
    /// `{ event: { type, properties } }`; session creation carries the
    /// cwd as `properties.info.directory`.
    #[test]
    fn envelope_extracts_opencode_bus_event_session_info() {
        let q = HookQuery {
            event: "session-start".into(),
            agent: Some("open-code".into()),
            ..Default::default()
        };
        let raw = serde_json::json!({
            "event": {
                "type": "session.created",
                "properties": {
                    "sessionID": "ses_bus",
                    "info": {
                        "id": "ses_bus",
                        "directory": "/home/user/ai-memory",
                        "title": "New session"
                    }
                }
            }
        });
        let env = HookEnvelope::from_query_and_body(q, raw);
        assert_eq!(env.session_id.as_deref(), Some("ses_bus"));
        assert_eq!(env.cwd.as_deref(), Some("/home/user/ai-memory"));
        assert_eq!(env.title_hint.as_deref(), Some("New session"));
    }

    /// Alternative agent-name spellings all map to the same canonical
    /// AgentKind. The hook scripts and the test e2e shim send slightly
    /// different strings for historical reasons; this asserts we
    /// remain forgiving.
    #[test]
    fn agent_name_aliases_all_map_correctly() {
        assert_eq!(parse_agent("claude-code"), AgentKind::ClaudeCode);
        assert_eq!(parse_agent("claude_code"), AgentKind::ClaudeCode);
        assert_eq!(parse_agent("claude"), AgentKind::ClaudeCode);
        assert_eq!(parse_agent("codex"), AgentKind::Codex);
        assert_eq!(parse_agent("opencode"), AgentKind::OpenCode);
        assert_eq!(parse_agent("open-code"), AgentKind::OpenCode);
        assert_eq!(parse_agent("cursor"), AgentKind::Cursor);
        assert_eq!(parse_agent("gemini-cli"), AgentKind::GeminiCli);
        assert_eq!(parse_agent("gemini"), AgentKind::GeminiCli);
        assert_eq!(parse_agent("claude-desktop"), AgentKind::ClaudeDesktop);
        assert_eq!(parse_agent("openclaw"), AgentKind::OpenClaw);
        assert_eq!(parse_agent("omp"), AgentKind::Omp);
        assert_eq!(parse_agent("pi"), AgentKind::Omp);
        assert_eq!(parse_agent("oh-my-pi"), AgentKind::Omp);
        // Anything else is `Other`. Critical for the hook router:
        // a typo in the query string must not crash, it just gets
        // attributed to the catch-all bucket.
        assert_eq!(parse_agent(""), AgentKind::Other);
        assert_eq!(parse_agent("CLAUDE-CODE"), AgentKind::Other); // case-sensitive on purpose
        assert_eq!(parse_agent("../../etc/passwd"), AgentKind::Other);
    }

    /// An empty body is legitimate (some hook events carry no
    /// payload). Envelope extraction must produce sane defaults
    /// rather than panicking.
    #[test]
    fn envelope_tolerates_empty_body() {
        let q = HookQuery {
            event: "stop".into(),
            agent: Some("claude-code".into()),
            ..Default::default()
        };
        let env = HookEnvelope::from_query_and_body(q, serde_json::json!({}));
        assert_eq!(env.event, HookEvent::Stop);
        assert!(env.session_id.is_none());
        assert!(env.cwd.is_none());
        assert!(env.title_hint.is_none());
        assert!(env.body_excerpt.is_none());
    }

    /// Body is well-formed JSON but the expected `session_id` /
    /// `cwd` keys are missing — extraction returns None per key.
    #[test]
    fn envelope_missing_expected_fields() {
        let q = HookQuery {
            event: "user-prompt".into(),
            agent: Some("claude-code".into()),
            ..Default::default()
        };
        let raw = serde_json::json!({ "garbage": 42 });
        let env = HookEnvelope::from_query_and_body(q, raw);
        assert_eq!(env.event, HookEvent::UserPrompt);
        assert!(env.session_id.is_none());
        assert!(env.cwd.is_none());
    }

    /// Body is a JSON primitive (string / null / number) rather
    /// than an object. The extractors must short-circuit cleanly.
    /// This guards against an upstream that POSTs a stringified
    /// payload by mistake.
    #[test]
    fn envelope_accepts_non_object_body() {
        let q = HookQuery {
            event: "post-tool-use".into(),
            agent: Some("claude-code".into()),
            ..Default::default()
        };
        for raw in [
            serde_json::json!(null),
            serde_json::json!("a stringy payload"),
            serde_json::json!(42),
            serde_json::json!([1, 2, 3]),
        ] {
            let env = HookEnvelope::from_query_and_body(q.clone(), raw);
            assert!(
                env.session_id.is_none(),
                "no session_id from non-object body"
            );
            assert!(env.cwd.is_none(), "no cwd from non-object body");
        }
    }

    /// Empty `agent` query param maps to Other (rather than panic
    /// or default to ClaudeCode). The hook router uses this for the
    /// attribution column, so we want it consistent.
    #[test]
    fn missing_agent_query_param_maps_to_other() {
        let q = HookQuery {
            event: "session-end".into(),
            agent: None,
            ..Default::default()
        };
        let env = HookEnvelope::from_query_and_body(q, serde_json::json!({}));
        assert_eq!(env.agent, AgentKind::Other);
    }

    /// Title-hint extraction must truncate at the first newline (the
    /// "first line" rule used everywhere in the wiki log + handoff
    /// surfaces) and cap at 80 chars to keep observation titles
    /// scannable in the log.md heading.
    #[test]
    fn user_prompt_title_truncates_at_newline_and_at_max_chars() {
        let q = HookQuery {
            event: "user-prompt".into(),
            agent: Some("claude-code".into()),
            ..Default::default()
        };
        // Multi-line prompt → title is the first line only.
        let env = HookEnvelope::from_query_and_body(
            q.clone(),
            serde_json::json!({ "prompt": "first line\nsecond line should be lost" }),
        );
        assert_eq!(env.title_hint.as_deref(), Some("first line"));

        // Very long single line → truncated with ellipsis.
        let long = "x".repeat(200);
        let env = HookEnvelope::from_query_and_body(q, serde_json::json!({ "prompt": long }));
        let title = env.title_hint.unwrap();
        assert!(title.chars().count() <= 80);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn post_tool_excerpt_truncates_without_splitting_utf8() {
        let q = HookQuery {
            event: "post-tool-use".into(),
            agent: Some("claude-code".into()),
            ..Default::default()
        };
        let output = format!("{}é", "x".repeat(1_999));
        let env = HookEnvelope::from_query_and_body(
            q,
            serde_json::json!({
                "tool": "bash",
                "result": output,
            }),
        );
        let excerpt = env.body_excerpt.unwrap();
        assert!(excerpt.ends_with('…'));
        assert!(excerpt.starts_with("tool: bash\n---\n"));
    }
}
