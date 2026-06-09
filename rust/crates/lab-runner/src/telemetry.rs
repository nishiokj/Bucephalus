//! Process-wide logging/tracing initialization for the core runner.
//!
//! The same binary serves two audiences:
//!   * the interactive `bucephalus` CLI, where logs are diagnostics a human
//!     reads on stderr, and
//!   * the Cloud worker's headless core runner, where logs must be
//!     machine-parseable and correlated into the originating request's trace.
//!
//! `BUCEPHALUS_LOG_FORMAT=json` selects the Cloud path: every event is emitted
//! as a single JSON line shaped for GCP Cloud Logging (`severity`, `time`,
//! `message`, and the `logging.googleapis.com/{trace,spanId}` correlation
//! fields). Any other value (or unset) selects a compact human formatter.
//!
//! All output goes to **stderr**. Stdout is reserved for the `--json` command
//! result protocol, so logging must never write there.
//!
//! Trace context is inherited from the environment (set by the Cloud worker
//! when it spawns the runner) so a run's runner logs join the same trace as the
//! API request and worker that scheduled it.

use std::io::Write;
use std::sync::Once;

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// `json` selects GCP Cloud Logging output; anything else is the human format.
pub const LOG_FORMAT_ENV: &str = "BUCEPHALUS_LOG_FORMAT";
/// Overrides the default level filter (accepts `RUST_LOG`-style directives).
pub const LOG_LEVEL_ENV: &str = "BUCEPHALUS_LOG_LEVEL";

/// Trace-context carriers, mirrored from the TypeScript control plane's headers
/// so a single run is one trace across API → worker → core runner.
pub const TRACE_ID_ENV: &str = "BUCEPHALUS_TRACE_ID";
pub const SPAN_ID_ENV: &str = "BUCEPHALUS_SPAN_ID";
pub const RUN_ID_ENV: &str = "BUCEPHALUS_RUN_ID";
pub const ATTEMPT_ID_ENV: &str = "BUCEPHALUS_ATTEMPT_ID";
/// Used to build the `projects/<id>/traces/<trace>` resource name GCP requires.
pub const PROJECT_ID_ENV: &str = "BUCEPHALUS_GCP_PROJECT_ID";
/// Logical component label attached to every line; defaults to `core-runner`.
pub const COMPONENT_ENV: &str = "BUCEPHALUS_LOG_COMPONENT";

static INIT: Once = Once::new();

/// Install the global tracing subscriber. Idempotent and safe to call from any
/// binary entrypoint; only the first call takes effect.
pub fn init() {
    INIT.call_once(|| {
        let json = std::env::var(LOG_FORMAT_ENV)
            .map(|value| value.eq_ignore_ascii_case("json"))
            .unwrap_or(false);

        let filter = EnvFilter::try_from_env(LOG_LEVEL_ENV)
            .or_else(|_| EnvFilter::try_from_default_env())
            .unwrap_or_else(|_| EnvFilter::new(if json { "info" } else { "warn" }));

        if json {
            let layer = GcpLayer {
                context: TraceContext::from_env(),
            };
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(layer)
                .try_init();
        } else {
            let layer = tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_target(false)
                .compact();
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(layer)
                .try_init();
        }
    });
}

/// Static per-process trace identity inherited from the environment.
struct TraceContext {
    project_id: Option<String>,
    trace_id: Option<String>,
    span_id: Option<String>,
    run_id: Option<String>,
    attempt_id: Option<String>,
    component: String,
}

impl TraceContext {
    fn from_env() -> Self {
        Self {
            project_id: non_empty_env(PROJECT_ID_ENV),
            trace_id: non_empty_env(TRACE_ID_ENV),
            span_id: non_empty_env(SPAN_ID_ENV),
            run_id: non_empty_env(RUN_ID_ENV),
            attempt_id: non_empty_env(ATTEMPT_ID_ENV),
            component: non_empty_env(COMPONENT_ENV).unwrap_or_else(|| "core-runner".to_string()),
        }
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

/// Emits each event as one GCP Cloud Logging JSON line on stderr.
struct GcpLayer {
    context: TraceContext,
}

impl<S> Layer<S> for GcpLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut fields = Map::new();
        event.record(&mut JsonVisitor(&mut fields));
        let metadata = event.metadata();
        let entry = self.context.build_entry(
            *metadata.level(),
            metadata.target(),
            chrono::Utc::now().to_rfc3339(),
            fields,
        );
        let line = Value::Object(entry).to_string();
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "{line}");
    }
}

impl TraceContext {
    /// Assemble one GCP Cloud Logging entry from an event's level, target,
    /// timestamp, and recorded fields. Pure so the wire shape is testable.
    fn build_entry(
        &self,
        level: Level,
        target: &str,
        time: String,
        mut fields: Map<String, Value>,
    ) -> Map<String, Value> {
        // tracing records the log message under the `message` field; hoist it
        // to the top-level key Cloud Logging displays.
        let message = match fields.remove("message") {
            Some(Value::String(text)) => text,
            Some(other) => other.to_string(),
            None => String::new(),
        };

        let mut entry = Map::new();
        entry.insert("time".into(), Value::String(time));
        entry.insert("severity".into(), Value::String(severity(level).to_string()));
        entry.insert("message".into(), Value::String(message));
        entry.insert("component".into(), Value::String(self.component.clone()));
        entry.insert("target".into(), Value::String(target.to_string()));

        if let Some(trace_id) = &self.trace_id {
            entry.insert("trace_id".into(), Value::String(trace_id.clone()));
            if let Some(project_id) = &self.project_id {
                entry.insert(
                    "logging.googleapis.com/trace".into(),
                    Value::String(format!("projects/{project_id}/traces/{trace_id}")),
                );
            }
        }
        if let Some(span_id) = &self.span_id {
            entry.insert("span_id".into(), Value::String(span_id.clone()));
            entry.insert(
                "logging.googleapis.com/spanId".into(),
                Value::String(span_id.clone()),
            );
        }
        if let Some(run_id) = &self.run_id {
            entry.insert("run_id".into(), Value::String(run_id.clone()));
        }
        if let Some(attempt_id) = &self.attempt_id {
            entry.insert("attempt_id".into(), Value::String(attempt_id.clone()));
        }

        // Remaining structured fields are merged at the top level as the
        // jsonPayload, without clobbering the reserved keys above.
        for (key, value) in fields {
            entry.entry(key).or_insert(value);
        }
        entry
    }
}

fn severity(level: Level) -> &'static str {
    match level {
        Level::ERROR => "ERROR",
        Level::WARN => "WARNING",
        Level::INFO => "INFO",
        Level::DEBUG => "DEBUG",
        Level::TRACE => "DEBUG",
    }
}

/// Collects an event's fields into a JSON map, preserving native scalar types.
struct JsonVisitor<'a>(&'a mut Map<String, Value>);

impl Visit for JsonVisitor<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .insert(field.name().to_string(), Value::String(format!("{value:?}")));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.0
            .insert(field.name().to_string(), Value::String(value.to_string()));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().to_string(), Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().to_string(), Value::from(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.0.insert(field.name().to_string(), Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().to_string(), Value::from(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn context() -> TraceContext {
        TraceContext {
            project_id: Some("my-proj".into()),
            trace_id: Some("abc123".into()),
            span_id: Some("span9".into()),
            run_id: Some("run-7".into()),
            attempt_id: Some("attempt-2".into()),
            component: "core-runner".into(),
        }
    }

    fn fields(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn maps_levels_to_gcp_severity() {
        assert_eq!(severity(Level::ERROR), "ERROR");
        assert_eq!(severity(Level::WARN), "WARNING");
        assert_eq!(severity(Level::INFO), "INFO");
        assert_eq!(severity(Level::DEBUG), "DEBUG");
        assert_eq!(severity(Level::TRACE), "DEBUG");
    }

    #[test]
    fn hoists_message_and_builds_trace_resource() {
        let entry = context().build_entry(
            Level::WARN,
            "lab_runner::experiment",
            "2026-06-09T00:00:00+00:00".into(),
            fields(&[
                ("message", json!("disk pressure")),
                ("free_mb", json!(128)),
            ]),
        );

        assert_eq!(entry["severity"], json!("WARNING"));
        assert_eq!(entry["message"], json!("disk pressure"));
        assert_eq!(entry["component"], json!("core-runner"));
        assert_eq!(
            entry["logging.googleapis.com/trace"],
            json!("projects/my-proj/traces/abc123")
        );
        assert_eq!(entry["logging.googleapis.com/spanId"], json!("span9"));
        assert_eq!(entry["run_id"], json!("run-7"));
        assert_eq!(entry["attempt_id"], json!("attempt-2"));
        // structured fields survive as jsonPayload, message is not duplicated.
        assert_eq!(entry["free_mb"], json!(128));
        assert!(!entry.contains_key("level"));
    }

    #[test]
    fn omits_trace_resource_without_project_id() {
        let mut ctx = context();
        ctx.project_id = None;
        let entry = ctx.build_entry(
            Level::INFO,
            "lab_runner",
            "2026-06-09T00:00:00+00:00".into(),
            Map::new(),
        );
        assert!(!entry.contains_key("logging.googleapis.com/trace"));
        // trace_id is still surfaced for searchability even without correlation.
        assert_eq!(entry["trace_id"], json!("abc123"));
    }

    #[test]
    fn event_fields_never_clobber_reserved_keys() {
        let entry = context().build_entry(
            Level::ERROR,
            "lab_runner",
            "t".into(),
            fields(&[("severity", json!("bogus")), ("component", json!("evil"))]),
        );
        assert_eq!(entry["severity"], json!("ERROR"));
        assert_eq!(entry["component"], json!("core-runner"));
    }
}
