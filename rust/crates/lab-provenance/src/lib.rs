use anyhow::{anyhow, Result};
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::Path;
use zip::write::FileOptions;

pub fn write_attestation(run_dir: &Path, payload: serde_json::Value) -> Result<()> {
    let path = run_dir.join("attestation.json");
    fs::write(path, serde_json::to_vec_pretty(&payload)?)?;
    Ok(())
}

pub fn default_attestation(
    resolved_digest: &str,
    image_digest: Option<&str>,
    grades: serde_json::Value,
    events_heads: Vec<(String, String)>,
    harness: serde_json::Value,
    trace_mode: &str,
) -> serde_json::Value {
    let heads: Vec<serde_json::Value> = events_heads
        .into_iter()
        .map(|(trial_id, head)| json!({"trial_id": trial_id, "head": head}))
        .collect();
    json!({
        "schema_version": "attestation_v1",
        "resolved_experiment_digest": resolved_digest,
        "image_digest": image_digest,
        "events_hashchain_heads": heads,
        "grades": grades,
        "harness": harness,
        "trace_ingestion": trace_mode,
    })
}

pub fn build_debug_bundle(run_dir: &Path, out_path: &Path) -> Result<()> {
    if out_path.exists() {
        return Err(anyhow!(
            "debug bundle output already exists: {}",
            out_path.display()
        ));
    }
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out_path)?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut included_files = Vec::new();
    let mut excluded_files = Vec::new();

    let include_paths = vec![
        run_dir.join("manifest.json"),
        run_dir.join("resolved_experiment.json"),
        run_dir.join("resolved_experiment.digest"),
        run_dir.join("attestation.json"),
    ];

    for p in include_paths {
        if p.exists() {
            let rel = p
                .strip_prefix(run_dir)
                .unwrap()
                .to_string_lossy()
                .to_string();
            write_debug_bundle_file(&mut zip, opts, run_dir, &p, &rel)?;
            included_files.push(rel);
        }
    }

    let trials_dir = run_dir.join("trials");
    if trials_dir.exists() {
        for entry in walkdir::WalkDir::new(&trials_dir) {
            let entry = entry?;
            if entry.file_type().is_file() {
                let path = entry.path();
                let rel = path
                    .strip_prefix(run_dir)
                    .unwrap()
                    .to_string_lossy()
                    .to_string();
                if should_include_debug_bundle_file(&rel) {
                    write_debug_bundle_file(&mut zip, opts, run_dir, path, &rel)?;
                    included_files.push(rel);
                } else {
                    excluded_files.push(rel);
                }
            }
        }
    }

    included_files.sort();
    excluded_files.sort();
    let manifest = json!({
        "schema_version": "bucephalus_debug_bundle_v1",
        "sensitivity": "support_bundle_review_before_sharing",
        "notice": "This bundle is for support/debugging. Structured JSON is redacted for common local-path and secret fields, but logs and agent outputs can still contain user data.",
        "redaction": {
            "json": "common secret and local path fields are replaced with placeholders",
            "excluded_by_default": [
                "trial workspaces",
                "temporary runtime directories",
                "state directories",
                "runtime directories",
                "secret-like filenames"
            ]
        },
        "included_files": included_files,
        "excluded_files": excluded_files,
    });
    zip.start_file("debug_bundle_manifest.json", opts)?;
    zip.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
    zip.finish()?;
    Ok(())
}

fn write_debug_bundle_file(
    zip: &mut zip::ZipWriter<fs::File>,
    opts: FileOptions,
    run_dir: &Path,
    path: &Path,
    rel: &str,
) -> Result<()> {
    zip.start_file(rel, opts)?;
    let data = if rel.ends_with(".json") {
        redact_debug_bundle_json_bytes(run_dir, &fs::read(path)?)
    } else {
        fs::read(path)?
    };
    zip.write_all(&data)?;
    Ok(())
}

fn redact_debug_bundle_json_bytes(run_dir: &Path, data: &[u8]) -> Vec<u8> {
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(data) else {
        return data.to_vec();
    };
    redact_debug_bundle_json_value(run_dir, &mut value);
    serde_json::to_vec_pretty(&value).unwrap_or_else(|_| data.to_vec())
}

fn redact_debug_bundle_json_value(run_dir: &Path, value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, child) in object.iter_mut() {
                if is_secretish_key(key) {
                    *child = serde_json::Value::String("<redacted>".to_string());
                } else if is_pathish_key(key) {
                    redact_debug_bundle_path_value(run_dir, child);
                } else {
                    redact_debug_bundle_json_value(run_dir, child);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                redact_debug_bundle_json_value(run_dir, item);
            }
        }
        serde_json::Value::String(raw) => {
            if looks_like_local_path(run_dir, raw) {
                *raw = "<local-path-redacted>".to_string();
            }
        }
        _ => {}
    }
}

fn redact_debug_bundle_path_value(run_dir: &Path, value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(raw) if looks_like_local_path(run_dir, raw) => {
            *raw = "<local-path-redacted>".to_string();
        }
        serde_json::Value::Array(items) => {
            for item in items {
                redact_debug_bundle_path_value(run_dir, item);
            }
        }
        serde_json::Value::Object(_) => redact_debug_bundle_json_value(run_dir, value),
        _ => {}
    }
}

fn is_secretish_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key == "authorization"
        || key == "password"
        || key == "token"
        || key == "access_token"
        || key == "refresh_token"
        || key == "api_key"
        || key == "secret"
        || key.contains("credential")
        || key.contains("password")
        || key.contains("token")
        || key.contains("secret")
}

fn is_pathish_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key == "path"
        || key == "cwd"
        || key == "workdir"
        || key.ends_with("_path")
        || key.ends_with("_dir")
        || key.ends_with("_root")
}

fn looks_like_local_path(run_dir: &Path, raw: &str) -> bool {
    let run_dir = run_dir.to_string_lossy();
    raw.contains(run_dir.as_ref())
        || raw.starts_with("/Users/")
        || raw.starts_with("/home/")
        || raw.starts_with("/private/")
        || raw.starts_with("/var/folders/")
}

fn should_include_debug_bundle_file(rel: &str) -> bool {
    let parts = rel.split('/').collect::<Vec<_>>();
    if parts.iter().any(|part| is_excluded_debug_bundle_part(part)) {
        return false;
    }
    let lower = rel.to_ascii_lowercase();
    !lower.ends_with(".env")
        && !lower.contains("/.env")
        && !lower.contains("secret")
        && !lower.contains("credential")
        && !lower.contains("token")
        && !lower.contains("apikey")
        && !lower.contains("api_key")
}

fn is_excluded_debug_bundle_part(part: &str) -> bool {
    matches!(part, "workspace" | "tmp" | "state" | "runtime")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "bucephalus_provenance_{}_{}_{}",
            label,
            std::process::id(),
            nanos
        ))
    }

    fn zip_names(path: &Path) -> Vec<String> {
        let file = fs::File::open(path).expect("open zip");
        let mut archive = zip::ZipArchive::new(file).expect("zip archive");
        let mut names = (0..archive.len())
            .map(|idx| archive.by_index(idx).expect("zip file").name().to_string())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    fn zip_text(path: &Path, name: &str) -> String {
        let file = fs::File::open(path).expect("open zip");
        let mut archive = zip::ZipArchive::new(file).expect("zip archive");
        let mut entry = archive.by_name(name).expect("zip entry");
        let mut text = String::new();
        entry.read_to_string(&mut text).expect("read entry");
        text
    }

    #[test]
    fn debug_bundle_redacts_json_and_excludes_sensitive_trial_files() {
        let root = temp_dir("debug_bundle");
        let run_dir = root.join("run_1");
        let trial_dir = run_dir.join("trials").join("trial_1");
        fs::create_dir_all(trial_dir.join("runner")).expect("runner dir");
        fs::create_dir_all(trial_dir.join("workspace")).expect("workspace dir");
        fs::create_dir_all(trial_dir.join("out")).expect("out dir");
        fs::write(
            run_dir.join("manifest.json"),
            serde_json::to_vec_pretty(&json!({
                "run_dir": run_dir.display().to_string(),
                "token": "secret-token",
                "container_path": "/bucephalus/out/result.json"
            }))
            .expect("manifest"),
        )
        .expect("write manifest");
        fs::write(
            trial_dir.join("runner").join("trial_runtime_state.json"),
            serde_json::to_vec_pretty(&json!({
                "workspace_dir": trial_dir.join("workspace").display().to_string(),
                "access_token": "access-token-value",
                "result_path": trial_dir.join("out").join("result.json").display().to_string()
            }))
            .expect("state"),
        )
        .expect("write state");
        fs::write(trial_dir.join("agent_stdout.log"), "hello from agent\n").expect("write stdout");
        fs::write(
            trial_dir.join("out").join("result.json"),
            r#"{"answer":{"summary":"ok"},"path":"/workspace/task/file.txt"}"#,
        )
        .expect("write result");
        fs::write(
            trial_dir.join("workspace").join("source.txt"),
            "private source",
        )
        .expect("write workspace");
        fs::write(trial_dir.join(".env"), "API_KEY=secret").expect("write env");

        let out_path = root.join("bundle.zip");
        build_debug_bundle(&run_dir, &out_path).expect("debug bundle");

        let names = zip_names(&out_path);
        assert!(names.contains(&"debug_bundle_manifest.json".to_string()));
        assert!(names.contains(&"manifest.json".to_string()));
        assert!(names.contains(&"trials/trial_1/agent_stdout.log".to_string()));
        assert!(names.contains(&"trials/trial_1/out/result.json".to_string()));
        assert!(names.contains(&"trials/trial_1/runner/trial_runtime_state.json".to_string()));
        assert!(!names.contains(&"trials/trial_1/workspace/source.txt".to_string()));
        assert!(!names.contains(&"trials/trial_1/.env".to_string()));

        let manifest = zip_text(&out_path, "manifest.json");
        assert!(!manifest.contains("secret-token"));
        assert!(!manifest.contains(&run_dir.display().to_string()));
        assert!(manifest.contains("<redacted>"));
        assert!(manifest.contains("<local-path-redacted>"));
        assert!(manifest.contains("/bucephalus/out/result.json"));

        let state = zip_text(&out_path, "trials/trial_1/runner/trial_runtime_state.json");
        assert!(!state.contains("access-token-value"));
        assert!(!state.contains(&trial_dir.display().to_string()));

        let bundle_manifest = zip_text(&out_path, "debug_bundle_manifest.json");
        assert!(bundle_manifest.contains("review_before_sharing"));
        assert!(bundle_manifest.contains("trials/trial_1/workspace/source.txt"));
        assert!(bundle_manifest.contains("trials/trial_1/.env"));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn debug_bundle_refuses_to_overwrite_existing_output() {
        let root = temp_dir("debug_bundle_overwrite");
        let run_dir = root.join("run_1");
        fs::create_dir_all(&run_dir).expect("run dir");
        fs::write(run_dir.join("manifest.json"), "{}").expect("manifest");
        let out_path = root.join("bundle.zip");
        fs::write(&out_path, "existing").expect("existing bundle");

        let err = build_debug_bundle(&run_dir, &out_path)
            .expect_err("existing bundle should not be overwritten")
            .to_string();
        assert!(err.contains("debug bundle output already exists"));

        fs::remove_dir_all(root).expect("cleanup");
    }
}
