//! View formatting helpers.
//!
//! The user-facing view catalog and layout contract live in `view_spec`.
//! This module keeps lower-level formatting utilities for identifiers and
//! JSON payload previews.

use serde_json::Value;

/// Compact long trial/task/run identifiers down to a recognizable tail.
///
/// `trial_xxxxxxxxxxxxxxxx_abcdef` -> `tr…abcdef` so a 32-char id stops
/// devouring 32 columns of every row. Short ids pass through unchanged.
pub fn compact_identifier(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let (tag, rest) = if let Some(rest) = trimmed.strip_prefix("trial_") {
        ("tr", rest)
    } else if let Some(rest) = trimmed.strip_prefix("task_") {
        ("tk", rest)
    } else if let Some(rest) = trimmed.strip_prefix("run_") {
        ("rn", rest)
    } else if let Some(rest) = trimmed.strip_prefix("variant_") {
        ("v", rest)
    } else {
        ("", trimmed)
    };
    let tail_len = 8;
    let count = rest.chars().count();
    if count <= tail_len {
        if tag.is_empty() {
            return trimmed.to_string();
        }
        return format!("{tag}:{rest}");
    }
    let tail: String = rest.chars().skip(count - tail_len).collect();
    if tag.is_empty() {
        format!("…{tail}")
    } else {
        format!("{tag}:…{tail}")
    }
}

/// Pretty-print a JSON Value for the detail pane (2-space indent,
/// strings unwrapped from quotes when single-line).
pub fn pretty_payload(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::String(s) => {
            // Try parsing as JSON in case the column is a stringified JSON blob.
            if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                return serde_json::to_string_pretty(&parsed).unwrap_or_else(|_| s.clone());
            }
            s.clone()
        }
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

/// Parse a payload value that may itself be a JSON-encoded string.
/// Returns the parsed value or the original when parsing fails.
fn coerce_payload(value: &Value) -> Value {
    match value {
        Value::String(s) => serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.clone())),
        other => other.clone(),
    }
}

fn deep_get<'v>(value: &'v Value, path: &[&str]) -> Option<&'v Value> {
    let mut current = value;
    for segment in path {
        current = current.get(segment)?;
    }
    Some(current)
}

/// Flatten a JSON value into a short one-line preview suitable for an
/// event-stream row. Strings stay as text. Objects/arrays render as a
/// compact `k=v k=v` summary or a `[n items]` placeholder.
fn flatten_for_preview(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.replace('\n', " ").trim().to_string(),
        Value::Array(items) => {
            if items.is_empty() {
                return String::new();
            }
            // Treat as a message-list if every item has role+content.
            if items
                .iter()
                .all(|i| i.get("role").is_some() && i.get("content").is_some())
            {
                let last = items.last().unwrap();
                let role = last.get("role").and_then(Value::as_str).unwrap_or("?");
                let content = last
                    .get("content")
                    .map(flatten_for_preview)
                    .unwrap_or_default();
                return format!("{}: {}", role, content);
            }
            let parts: Vec<String> = items
                .iter()
                .take(3)
                .map(flatten_for_preview)
                .filter(|s| !s.is_empty())
                .collect();
            if parts.is_empty() {
                format!("[{} items]", items.len())
            } else if items.len() > parts.len() {
                format!("{} … (+{})", parts.join(" · "), items.len() - parts.len())
            } else {
                parts.join(" · ")
            }
        }
        Value::Object(map) => {
            // If the object looks like {type, text} (anthropic content block), unwrap.
            if let Some(text) = map.get("text").and_then(Value::as_str) {
                return text.replace('\n', " ").trim().to_string();
            }
            let pairs: Vec<String> = map
                .iter()
                .take(4)
                .map(|(k, v)| {
                    let flat = flatten_for_preview(v);
                    if flat.is_empty() {
                        format!("{k}=·")
                    } else {
                        format!("{k}={flat}")
                    }
                })
                .collect();
            pairs.join(" ")
        }
    }
}

/// Build a one-line content preview for an event row by digging into
/// the payload for the field most likely to carry semantic content
/// (tool args, tool result, response text, error message, …).
///
/// Returns None when no useful content is found; the caller should fall
/// back to leaving the second line blank rather than dumping JSON.
pub fn event_content_preview(event_type: &str, payload: &Value) -> Option<String> {
    let parsed = coerce_payload(payload);
    // Some emitters nest the real fields under `payload.*`; try both.
    let bases: [&Value; 2] = match parsed.get("payload") {
        Some(inner) => [&parsed, inner],
        None => [&parsed, &parsed],
    };

    let candidates: &[&[&str]] = match event_type {
        "tool_call_start" => &[
            &["tool", "input"],
            &["tool", "arguments"],
            &["tool", "args"],
            &["input"],
            &["arguments"],
        ],
        "tool_call_end" => &[
            &["tool", "output"],
            &["tool", "result"],
            &["output"],
            &["result"],
            &["stdout"],
            &["text"],
        ],
        "model_call_start" => &[
            &["messages"],
            &["prompt"],
            &["system"],
            &["input"],
            &["text"],
        ],
        "model_call_end" => &[
            &["response", "text"],
            &["response", "content"],
            &["text"],
            &["content"],
            &["output"],
            &["completion"],
            &["message", "content"],
        ],
        "task_start" => &[
            &["task", "description"],
            &["description"],
            &["task_id"],
            &["prompt"],
        ],
        "task_end" => &[
            &["outcome", "message"],
            &["outcome", "summary"],
            &["error", "message"],
            &["error"],
            &["message"],
        ],
        _ => &[
            &["message"],
            &["text"],
            &["content"],
            &["error", "message"],
            &["error"],
            &["description"],
            &["summary"],
        ],
    };

    for base in &bases {
        for path in candidates {
            if let Some(val) = deep_get(base, path) {
                let flat = flatten_for_preview(val);
                if !flat.is_empty() {
                    return Some(flat);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_identifier_clips_long_ids() {
        // Long trial id keeps an 8-char tail with the `tr` tag.
        assert_eq!(
            compact_identifier("trial_2d3b40e1c7f3aa11b9b15ad6"),
            "tr:…b9b15ad6"
        );
    }

    #[test]
    fn compact_identifier_passes_short_ids() {
        assert_eq!(compact_identifier("task_1"), "tk:1");
        assert_eq!(compact_identifier("variant_a"), "v:a");
        assert_eq!(compact_identifier("base"), "base");
    }

    #[test]
    fn event_content_preview_extracts_tool_input() {
        let payload = serde_json::json!({
            "tool": {
                "name": "bash",
                "input": {"command": "ls -la /tmp"}
            }
        });
        let preview = event_content_preview("tool_call_start", &payload).unwrap();
        assert!(preview.contains("command=ls -la /tmp"), "got: {preview}");
    }

    #[test]
    fn event_content_preview_extracts_response_text() {
        let payload = serde_json::json!({
            "response": {"text": "the answer is 42"}
        });
        assert_eq!(
            event_content_preview("model_call_end", &payload).as_deref(),
            Some("the answer is 42")
        );
    }

    #[test]
    fn event_content_preview_handles_stringified_payload() {
        let payload = Value::String(r#"{"text": "hi there"}"#.to_string());
        assert_eq!(
            event_content_preview("model_call_end", &payload).as_deref(),
            Some("hi there")
        );
    }
}
