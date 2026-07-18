use serde::de::DeserializeOwned;

use tracing::warn;

use crate::error::{LlmError, LlmResult};
use crate::text::truncate_with_ellipsis;

/// Generous cap for successful provider responses. Normal chat, structured
/// output, SSE transcripts, and embedding payloads are far smaller; this only
/// protects us from broken or hostile provider endpoints buffering forever.
pub(crate) const MAX_PROVIDER_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_PROVIDER_ERROR_BYTES: usize = 1024 * 1024;
const DISPLAY_ERROR_BYTES: usize = 1024;

pub(crate) async fn response_bytes_limited(
    mut resp: reqwest::Response,
    max_bytes: usize,
) -> LlmResult<Vec<u8>> {
    let status = resp.status().as_u16();
    let mut bytes = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(LlmError::from)? {
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(LlmError::Provider {
                status,
                body: format!("provider response exceeded {max_bytes} bytes"),
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(crate) async fn response_text_limited(resp: reqwest::Response) -> LlmResult<String> {
    let bytes = response_bytes_limited(resp, MAX_PROVIDER_RESPONSE_BYTES).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub(crate) async fn response_json_limited<T: DeserializeOwned>(
    resp: reqwest::Response,
) -> LlmResult<T> {
    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = response_bytes_limited(resp, MAX_PROVIDER_RESPONSE_BYTES).await?;
    parse_json_response(status, &content_type, &bytes)
}

/// Like [`response_json_limited`] but, when the upstream returns HTTP 200
/// with a body that is not JSON (e.g. an HTML 502 gateway error page served
/// with a 200 status, or an empty body), surfaces a clear [`LlmError::Provider`]
/// carrying the status and a body snippet instead of a cryptic `serde` /
/// `unexpected response shape` error. This is the common case for misbehaving
/// OpenAI-compatible proxies that wrap failures in a 200 response.
pub(crate) async fn response_json_or_provider_error<T: DeserializeOwned>(
    resp: reqwest::Response,
) -> LlmResult<T> {
    let status = resp.status().as_u16();
    let is_success = resp.status().is_success();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = response_bytes_limited(resp, MAX_PROVIDER_RESPONSE_BYTES).await?;
    let text = String::from_utf8_lossy(&bytes);
    // A non-2xx status (e.g. a 502 Bad Gateway proxy error that still carries
    // a JSON-looking body) is always a provider error first — never a parse
    // failure. Gateways frequently wrap failures in a body that starts with
    // `{` (a truncated / error JSON), which would otherwise trip the `looks_json`
    // heuristic and surface a cryptic `serde` error instead of the real status.
    if !is_success {
        warn!(
            status,
            content_type = %content_type,
            body_len = bytes.len(),
            "LLM endpoint returned a non-2xx status; treating as provider error"
        );
        return Err(LlmError::Provider {
            status,
            body: truncate_with_ellipsis(text.as_ref(), DISPLAY_ERROR_BYTES),
        });
    }
    // A JSON endpoint should answer with JSON. If the body is empty or looks
    // like HTML (or any non-JSON content type) on a 2xx status, the gateway
    // likely returned an error page instead of a real response.
    let looks_json = content_type.contains("json")
        || text.trim_start().starts_with('{')
        || text.trim_start().starts_with('[');
    if !looks_json {
        warn!(
            status,
            content_type = %content_type,
            body_len = bytes.len(),
            "LLM endpoint returned a non-JSON body on a 2xx status; treating as provider error"
        );
        return Err(LlmError::Provider {
            status,
            body: truncate_with_ellipsis(text.as_ref(), DISPLAY_ERROR_BYTES),
        });
    }
    parse_json_response(status, &content_type, &bytes)
}

/// Deserialize `bytes` into `T`, but never surface a cryptic `serde` parse
/// error when the body describes a failure. A response from a misbehaving
/// gateway/proxy can carry a `{"error": "..."}` envelope (sometimes itself
/// quoting a downstream parse failure, e.g.
/// `{"error":"serde: expected ... at line 30"}`), or a corrupted/truncated
/// envelope whose string value contains a control character
/// (`{"error":"serde: control character ... at line 31"}`). In those cases we
/// surface a clear [`LlmError::Provider`] carrying the status and the error
/// message rather than a `serde: ...` stack that hides the real cause. A body
/// that is neither valid JSON nor a recoverable error envelope keeps the
/// `Serde` variant, which the callers already treat as retryable.
fn parse_json_response<T: DeserializeOwned>(
    status: u16,
    content_type: &str,
    bytes: &[u8],
) -> LlmResult<T> {
    match serde_json::from_slice::<T>(bytes) {
        Ok(parsed) => Ok(parsed),
        Err(e) => {
            // If the body is itself a JSON error envelope, prefer it.
            if let Ok(envelope) = serde_json::from_slice::<serde_json::Value>(bytes)
                && let Some(message) = error_message_from_envelope(envelope)
            {
                warn!(
                    status,
                    content_type = %content_type,
                    body_len = bytes.len(),
                    error = %message,
                    "LLM endpoint returned a JSON error envelope"
                );
                return Err(LlmError::Provider {
                    status,
                    body: truncate_with_ellipsis(&message, DISPLAY_ERROR_BYTES),
                });
            }
            // The body may be a *truncated* or control-character-corrupted JSON
            // error envelope. A strict parse fails, so scan the raw bytes
            // leniently for an error key and recover whatever message prefix we
            // can before the corruption.
            if let Some(message) = lenient_error_message(bytes) {
                warn!(
                    status,
                    content_type = %content_type,
                    body_len = bytes.len(),
                    error = %message,
                    "LLM endpoint returned a corrupted JSON error envelope"
                );
                return Err(LlmError::Provider {
                    status,
                    body: truncate_with_ellipsis(&message, DISPLAY_ERROR_BYTES),
                });
            }
            parse_failure(status, content_type, bytes, e)
        }
    }
}

/// Log and surface a genuine JSON parse failure as a retryable `Serde` error.
fn parse_failure<T: DeserializeOwned>(
    status: u16,
    content_type: &str,
    bytes: &[u8],
    e: serde_json::Error,
) -> LlmResult<T> {
    warn!(
        status,
        content_type = %content_type,
        body_len = bytes.len(),
        error = %e,
        "failed to parse LLM JSON response"
    );
    Err(LlmError::from(e))
}

/// Extract a human-readable error string from a JSON error envelope of the
/// common shapes `{"error":"msg"}`, `{"error":{"message":"msg"}}`,
/// `{"error":"msg","message":"msg"}`, and `{"detail":"msg"}`.
fn error_message_from_envelope(value: serde_json::Value) -> Option<String> {
    let obj = value.as_object()?;
    let error = obj.get("error");
    match error {
        Some(serde_json::Value::String(s)) => return Some(s.clone()),
        Some(serde_json::Value::Object(inner)) => {
            if let Some(m) = inner.get("message").and_then(|v| v.as_str()) {
                return Some(m.to_string());
            }
        }
        _ => {}
    }
    if let Some(m) = obj.get("message").and_then(|v| v.as_str()) {
        return Some(m.to_string());
    }
    if let Some(d) = obj.get("detail").and_then(|v| v.as_str()) {
        return Some(d.to_string());
    }
    None
}

/// Leniently recover an error message from a *corrupted* JSON body that a
/// strict parser rejects — e.g. a gateway that truncates the response mid-string
/// or injects a control character (`{"error":"serde: control character ... at
/// line 31"}`). Rather than fail the whole parse, we scan the raw bytes for a
/// known error key (`error` / `message` / `detail`) and copy the following
/// quoted string up to the first control character or closing quote. This
/// trades completeness for a usable, human-readable message.
fn lenient_error_message(bytes: &[u8]) -> Option<String> {
    for key in ["error", "message", "detail"] {
        let mut needle = b"\"".to_vec();
        needle.extend_from_slice(key.as_bytes());
        needle.push(b'"');
        let mut i = 0;
        while let Some(pos) = find_subslice(bytes, &needle, i) {
            // Skip optional whitespace and a single colon after the key.
            let mut j = pos + needle.len();
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if j >= bytes.len() || bytes[j] != b':' {
                i = pos + 1;
                continue;
            }
            j += 1;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if j >= bytes.len() || bytes[j] != b'"' {
                i = pos + 1;
                continue;
            }
            // Read the string value, stopping at an unescaped closing quote or
            // any control character (the corruption point).
            j += 1;
            let mut value = String::new();
            while j < bytes.len() {
                let b = bytes[j];
                if b == b'\\' && j + 1 < bytes.len() {
                    let next = bytes[j + 1];
                    match next {
                        b'"' => value.push('"'),
                        b'\\' => value.push('\\'),
                        b'/' => value.push('/'),
                        b'n' => value.push('\n'),
                        b't' => value.push('\t'),
                        b'r' => value.push('\r'),
                        _ => value.push('?'),
                    }
                    j += 2;
                    continue;
                }
                if b == b'"' || b.is_ascii_control() {
                    break;
                }
                value.push(b as char);
                j += 1;
            }
            if !value.is_empty() {
                return Some(value);
            }
            i = pos + 1;
        }
    }
    None
}

/// Find `needle` in `haystack` starting at `from`, returning the first byte
/// index or `None`.
fn find_subslice(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from + needle.len() > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

pub(crate) async fn provider_error_body(resp: reqwest::Response) -> String {
    match response_bytes_limited(resp, MAX_PROVIDER_ERROR_BYTES).await {
        Ok(bytes) => {
            let body = String::from_utf8_lossy(&bytes);
            truncate_with_ellipsis(body.as_ref(), DISPLAY_ERROR_BYTES)
        }
        Err(LlmError::Provider { body, .. }) => body,
        Err(e) => format!("<failed to read provider error body: {e}>"),
    }
}
