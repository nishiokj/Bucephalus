use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::model::MetricDefinition;
use crate::persistence::backend::{open_event_row_store, EventRowStore};
use crate::persistence::rows::{EventRow, MetricRow, VariantSnapshotRow};

const LIVE_EVENT_INGEST_INTERVAL: Duration = Duration::from_millis(500);

pub(crate) fn load_event_rows(
    events_path: &Path,
    run_id: &str,
    trial_id: &str,
    schedule_idx: usize,
    variant_id: &str,
    task_id: &str,
    repl_idx: usize,
) -> Result<Vec<EventRow>> {
    let mut rows = Vec::new();
    let file = fs::File::open(events_path)?;
    let reader = BufReader::new(file);
    for (seq, line) in reader.lines().enumerate() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (event_type, ts, payload) = match serde_json::from_str::<Value>(trimmed) {
            Ok(payload) => {
                let event_type = payload
                    .get("event_type")
                    .and_then(Value::as_str)
                    .or_else(|| payload.get("type").and_then(Value::as_str))
                    .unwrap_or("unknown")
                    .to_string();
                let ts = payload
                    .get("ts")
                    .and_then(Value::as_str)
                    .or_else(|| payload.get("timestamp").and_then(Value::as_str))
                    .map(str::to_string);
                (event_type, ts, payload)
            }
            Err(err) => (
                "trajectory_parse_error".to_string(),
                None,
                json!({
                    "event_type": "trajectory_parse_error",
                    "error": err.to_string(),
                    "raw_line": trimmed,
                }),
            ),
        };
        rows.push(EventRow {
            run_id: run_id.to_string(),
            trial_id: trial_id.to_string(),
            schedule_idx,
            slot_commit_id: String::new(),
            attempt: 0,
            row_seq: seq,
            variant_id: variant_id.to_string(),
            task_id: task_id.to_string(),
            repl_idx,
            seq,
            event_type,
            ts,
            payload,
        });
    }
    Ok(rows)
}

#[derive(Debug, Clone)]
pub(crate) struct LiveEventIngestRequest {
    pub(crate) run_dir: std::path::PathBuf,
    pub(crate) events_path: std::path::PathBuf,
    pub(crate) run_id: String,
    pub(crate) trial_id: String,
    pub(crate) schedule_idx: usize,
    pub(crate) variant_id: String,
    pub(crate) task_id: String,
    pub(crate) repl_idx: usize,
    pub(crate) attempt: usize,
}

pub(crate) struct LiveEventIngestHandle {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<Result<usize>>>,
}

impl LiveEventIngestHandle {
    pub(crate) fn stop(mut self) -> Result<usize> {
        self.stop_and_join()
    }

    fn stop_and_join(&mut self) -> Result<usize> {
        self.stop.store(true, Ordering::Relaxed);
        let join = self
            .join
            .take()
            .ok_or_else(|| anyhow!("live event ingest thread already joined"))?;
        join.join()
            .map_err(|_| anyhow!("live event ingest thread panicked"))?
    }
}

impl Drop for LiveEventIngestHandle {
    fn drop(&mut self) {
        if self.join.is_none() {
            return;
        }
        if let Err(err) = self.stop_and_join() {
            eprintln!("warning: live event ingest shutdown failed: {}", err);
        }
    }
}

pub(crate) fn spawn_live_event_ingest(request: LiveEventIngestRequest) -> LiveEventIngestHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let join = thread::spawn(move || live_event_ingest_loop(request, thread_stop));
    LiveEventIngestHandle {
        stop,
        join: Some(join),
    }
}

fn live_event_ingest_loop(request: LiveEventIngestRequest, stop: Arc<AtomicBool>) -> Result<usize> {
    let mut sink = open_event_row_store(&request.run_dir)?;
    let mut next_row_seq = 0usize;
    loop {
        next_row_seq = ingest_new_event_rows(&request, &mut *sink, next_row_seq)?;
        sink.flush()?;
        if stop.load(Ordering::Relaxed) {
            next_row_seq = ingest_new_event_rows(&request, &mut *sink, next_row_seq)?;
            sink.flush()?;
            return Ok(next_row_seq);
        }
        thread::sleep(LIVE_EVENT_INGEST_INTERVAL);
    }
}

fn ingest_new_event_rows(
    request: &LiveEventIngestRequest,
    sink: &mut dyn EventRowStore,
    next_row_seq: usize,
) -> Result<usize> {
    if !request.events_path.exists() {
        return Ok(next_row_seq);
    }
    let rows = load_event_rows(
        &request.events_path,
        &request.run_id,
        &request.trial_id,
        request.schedule_idx,
        &request.variant_id,
        &request.task_id,
        request.repl_idx,
    )?;
    let new_rows = rows
        .into_iter()
        .filter(|row| row.row_seq >= next_row_seq)
        .map(|mut row| {
            row.attempt = request.attempt;
            row
        })
        .collect::<Vec<_>>();
    if new_rows.is_empty() {
        return Ok(next_row_seq);
    }
    let next = new_rows
        .iter()
        .map(|row| row.row_seq)
        .max()
        .map(|row_seq| row_seq + 1)
        .unwrap_or(next_row_seq);
    sink.append_event_rows(&new_rows)?;
    Ok(next)
}

pub(crate) fn build_metric_rows(
    run_id: &str,
    trial_id: &str,
    schedule_idx: usize,
    variant_id: &str,
    task_id: &str,
    repl_idx: usize,
    outcome: &str,
    metrics: &Value,
    primary_metric_name: &str,
    primary_metric_value: &Value,
) -> Vec<MetricRow> {
    let mut rows = Vec::new();
    if let Some(metric_obj) = metrics.as_object() {
        for (row_seq, (metric_name, metric_value)) in metric_obj.iter().enumerate() {
            rows.push(MetricRow {
                run_id: run_id.to_string(),
                trial_id: trial_id.to_string(),
                schedule_idx,
                slot_commit_id: String::new(),
                attempt: 0,
                row_seq,
                variant_id: variant_id.to_string(),
                task_id: task_id.to_string(),
                repl_idx,
                outcome: outcome.to_string(),
                metric_name: metric_name.clone(),
                metric_value: metric_value.clone(),
                metric_source: None,
            });
        }
    }
    rows.push(MetricRow {
        run_id: run_id.to_string(),
        trial_id: trial_id.to_string(),
        schedule_idx,
        slot_commit_id: String::new(),
        attempt: 0,
        row_seq: rows.len(),
        variant_id: variant_id.to_string(),
        task_id: task_id.to_string(),
        repl_idx,
        outcome: outcome.to_string(),
        metric_name: primary_metric_name.to_string(),
        metric_value: primary_metric_value.clone(),
        metric_source: Some("primary".to_string()),
    });
    rows
}

fn declared_metric_value(
    definition: &MetricDefinition,
    agent_response_payload: &Value,
) -> Option<Value> {
    match definition.source.source_type.as_str() {
        "agent_response" => {
            let pointer = definition.source.pointer.as_deref()?;
            agent_response_payload.pointer(pointer).cloned()
        }
        "runtime_output" => {
            let output = definition
                .definition_json
                .pointer("/source/output")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            let output_id = output.strip_prefix("agent.").unwrap_or(output);
            if output_id != "result" {
                return None;
            }
            if let Some(pointer) = definition.source.pointer.as_deref() {
                agent_response_payload.pointer(pointer).cloned()
            } else {
                Some(agent_response_payload.clone())
            }
        }
        _ => None,
    }
}

pub(crate) fn extract_declared_metrics(
    definitions: &[MetricDefinition],
    agent_response_payload: &Value,
) -> (Value, Option<(String, Value)>) {
    let mut metrics = serde_json::Map::new();
    let mut primary = None;

    for definition in definitions {
        let value = declared_metric_value(definition, agent_response_payload);
        if let Some(value) = value.or_else(|| definition.required.then(|| json!(null))) {
            if definition.primary && primary.is_none() {
                primary = Some((definition.id.clone(), value.clone()));
            }
            metrics.insert(definition.id.clone(), value);
        } else if definition.primary && primary.is_none() {
            primary = Some((definition.id.clone(), json!(null)));
        }
    }

    (Value::Object(metrics), primary)
}

pub(crate) fn build_variant_snapshot_rows(
    run_id: &str,
    trial_id: &str,
    schedule_idx: usize,
    variant_id: &str,
    baseline_id: &str,
    task_id: &str,
    repl_idx: usize,
    bindings: &Value,
) -> Vec<VariantSnapshotRow> {
    let mut rows = Vec::new();
    if let Some(bindings_obj) = bindings.as_object() {
        for (row_seq, (binding_name, binding_value)) in bindings_obj.iter().enumerate() {
            rows.push(VariantSnapshotRow {
                run_id: run_id.to_string(),
                trial_id: trial_id.to_string(),
                schedule_idx,
                slot_commit_id: String::new(),
                attempt: 0,
                row_seq,
                variant_id: variant_id.to_string(),
                baseline_id: baseline_id.to_string(),
                task_id: task_id.to_string(),
                repl_idx,
                binding_name: binding_name.clone(),
                binding_value: binding_value.clone(),
                binding_value_text: binding_value_to_text(binding_value),
            });
        }
    }
    rows
}

fn binding_value_to_text(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(v) => v.to_string(),
        Value::Number(v) => v.to_string(),
        Value::String(v) => v.clone(),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value).unwrap_or_default(),
    }
}
