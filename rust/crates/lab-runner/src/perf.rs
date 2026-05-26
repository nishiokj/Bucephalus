use anyhow::Result;
use chrono::Utc;
use serde_json::{Map, Value, json};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::persistence::store::SqliteRunStore;
use crate::util::env_var_with_legacy;

static SAMPLE_SEQ: AtomicU64 = AtomicU64::new(1);
static SQLITE_PERF_WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub const CLI_INVOKED_AT_MS_ENV: &str = "BUCEPHALUS_CLI_INVOKED_AT_MS";
const PERF_CAPTURE_ENV: &str = "BUCEPHALUS_PERF_CAPTURE";
const PERF_RESOURCE_SAMPLING_ENV: &str = "BUCEPHALUS_PERF_RESOURCE_SAMPLING";

fn enabled() -> bool {
    env_var_with_legacy(PERF_CAPTURE_ENV)
        .map(|value| !matches!(value.trim(), "0" | "false" | "off" | "no"))
        .unwrap_or(true)
}

fn resource_sampling_enabled() -> bool {
    env_var_with_legacy(PERF_RESOURCE_SAMPLING_ENV)
        .map(|value| matches!(value.trim(), "1" | "true" | "on" | "yes"))
        .unwrap_or(false)
}

pub(crate) fn unix_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn process_rss_kb() -> Option<i64> {
    if !resource_sampling_enabled() {
        return None;
    }
    let pid = std::process::id().to_string();
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", pid.as_str()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<i64>()
        .ok()
}

fn docker_container_stats(container_id: &str) -> Option<Value> {
    if !resource_sampling_enabled() || container_id == "host" {
        return None;
    }
    let output = Command::new("docker")
        .args([
            "stats",
            "--no-stream",
            "--format",
            "{{json .}}",
            container_id,
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(raw.trim()).ok()
}

fn insert_optional_string(map: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        map.insert(key.to_string(), json!(value));
    }
}

fn insert_optional_usize(map: &mut Map<String, Value>, key: &str, value: Option<usize>) {
    if let Some(value) = value {
        map.insert(key.to_string(), json!(value));
    }
}

pub(crate) struct PerfRecord<'a> {
    pub(crate) run_dir: &'a Path,
    pub(crate) run_id: &'a str,
    pub(crate) trial_id: Option<&'a str>,
    pub(crate) schedule_idx: Option<usize>,
    pub(crate) attempt: Option<usize>,
    pub(crate) sample_kind: &'a str,
    pub(crate) stage: &'a str,
    pub(crate) duration_ms: Option<f64>,
    pub(crate) detail: Value,
}

pub(crate) fn record(record: PerfRecord<'_>) -> Result<()> {
    if !enabled() {
        return Ok(());
    }
    let seq = SAMPLE_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut payload = Map::new();
    payload.insert("schema_version".to_string(), json!("performance_sample_v1"));
    payload.insert(
        "sample_id".to_string(),
        json!(format!(
            "{}:{}:{}:{}",
            record.run_id, record.stage, record.sample_kind, seq
        )),
    );
    payload.insert("run_id".to_string(), json!(record.run_id));
    insert_optional_string(&mut payload, "trial_id", record.trial_id);
    insert_optional_usize(&mut payload, "schedule_idx", record.schedule_idx);
    insert_optional_usize(&mut payload, "attempt", record.attempt);
    payload.insert("sample_seq".to_string(), json!(seq));
    payload.insert("sample_kind".to_string(), json!(record.sample_kind));
    payload.insert("stage".to_string(), json!(record.stage));
    if let Some(duration_ms) = record.duration_ms {
        payload.insert("duration_ms".to_string(), json!(duration_ms));
    }
    if let Some(rss_kb) = process_rss_kb() {
        payload.insert("process_rss_kb".to_string(), json!(rss_kb));
    }
    payload.insert("recorded_at".to_string(), json!(Utc::now().to_rfc3339()));
    payload.insert("recorded_at_ms".to_string(), json!(unix_time_ms()));
    payload.insert("detail".to_string(), record.detail);

    let payload = Value::Object(payload);
    let write_result = SQLITE_PERF_WRITE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|err| anyhow::anyhow!("performance sample write lock poisoned: {}", err))
        .and_then(|_guard| {
            SqliteRunStore::open(record.run_dir)
                .and_then(|mut store| store.upsert_performance_sample(&payload))
        });
    if let Err(err) = write_result {
        eprintln!(
            "bucephalus performance capture skipped for stage '{}': {}",
            record.stage, err
        );
    }
    Ok(())
}

pub(crate) fn record_duration(
    run_dir: &Path,
    run_id: &str,
    trial_id: Option<&str>,
    schedule_idx: Option<usize>,
    attempt: Option<usize>,
    stage: &str,
    started: Instant,
    detail: Value,
) -> Result<()> {
    record(PerfRecord {
        run_dir,
        run_id,
        trial_id,
        schedule_idx,
        attempt,
        sample_kind: "duration",
        stage,
        duration_ms: Some(started.elapsed().as_secs_f64() * 1000.0),
        detail,
    })
}

pub(crate) fn record_event(
    run_dir: &Path,
    run_id: &str,
    trial_id: Option<&str>,
    schedule_idx: Option<usize>,
    attempt: Option<usize>,
    stage: &str,
    detail: Value,
) -> Result<()> {
    record(PerfRecord {
        run_dir,
        run_id,
        trial_id,
        schedule_idx,
        attempt,
        sample_kind: "event",
        stage,
        duration_ms: None,
        detail,
    })
}

pub(crate) fn record_cli_latency(
    run_dir: &Path,
    run_id: &str,
    stage: &str,
    detail: Value,
) -> Result<()> {
    let Some(started_at_ms) = env_var_with_legacy(CLI_INVOKED_AT_MS_ENV)
        .ok()
        .and_then(|raw| raw.parse::<i64>().ok())
    else {
        return Ok(());
    };
    let duration_ms = (unix_time_ms() - started_at_ms).max(0) as f64;
    record(PerfRecord {
        run_dir,
        run_id,
        trial_id: None,
        schedule_idx: None,
        attempt: None,
        sample_kind: "duration",
        stage,
        duration_ms: Some(duration_ms),
        detail,
    })
}

pub(crate) fn record_container_stats(
    run_dir: &Path,
    run_id: &str,
    trial_id: &str,
    schedule_idx: usize,
    attempt: usize,
    stage: &str,
    container_id: &str,
    role: &str,
) -> Result<()> {
    if !resource_sampling_enabled() {
        return Ok(());
    }
    record_event(
        run_dir,
        run_id,
        Some(trial_id),
        Some(schedule_idx),
        Some(attempt),
        stage,
        json!({
            "container_id": container_id,
            "role": role,
            "docker_stats": docker_container_stats(container_id)
        }),
    )
}
