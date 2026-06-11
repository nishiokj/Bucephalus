use anyhow::{anyhow, Result};
use std::path::PathBuf;

pub const BUCEPHALUS_DB_ENV: &str = "BUCEPHALUS_DB";
pub const BUCEPHALUS_HOME_ENV: &str = "BUCEPHALUS_HOME";
pub const ACCOUNT_SQLITE_FILE: &str = "bucephalus.sqlite";

fn require_absolute_env_path(name: &str) -> Result<Option<PathBuf>> {
    let Some(raw) = std::env::var_os(name) else {
        return Ok(None);
    };
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(anyhow!("{} must be an absolute path", name));
    }
    Ok(Some(path))
}

pub fn bucephalus_home() -> Result<PathBuf> {
    if let Some(path) = require_absolute_env_path(BUCEPHALUS_HOME_ENV)? {
        return Ok(path);
    }

    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("HOME is not set; set {}", BUCEPHALUS_HOME_ENV))?;
        Ok(home
            .join("Library")
            .join("Application Support")
            .join("Bucephalus"))
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
            Ok(appdata.join("Bucephalus"))
        } else {
            let home = std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .ok_or_else(|| {
                    anyhow!(
                        "APPDATA and USERPROFILE are not set; set {}",
                        BUCEPHALUS_HOME_ENV
                    )
                })?;
            Ok(home.join("AppData").join("Roaming").join("Bucephalus"))
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(data_home) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
            Ok(data_home.join("bucephalus"))
        } else {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| anyhow!("HOME is not set; set {}", BUCEPHALUS_HOME_ENV))?;
            Ok(home.join(".local").join("share").join("bucephalus"))
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", unix)))]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("HOME is not set; set {}", BUCEPHALUS_HOME_ENV))?;
        Ok(home.join(".bucephalus"))
    }
}

pub fn account_sqlite_path() -> Result<PathBuf> {
    if let Some(path) = require_absolute_env_path(BUCEPHALUS_DB_ENV)? {
        return Ok(path);
    }
    Ok(bucephalus_home()?.join(ACCOUNT_SQLITE_FILE))
}

pub fn default_run_root() -> Result<PathBuf> {
    Ok(bucephalus_home()?.join("runs"))
}

pub fn default_build_root() -> Result<PathBuf> {
    Ok(bucephalus_home()?.join("builds"))
}

pub fn default_agent_root() -> Result<PathBuf> {
    Ok(bucephalus_home()?.join("agents"))
}

/// Persisted Cloud connection profile. `bucephalus login` writes it once;
/// every Cloud-facing command resolves its API URL and OAuth parameters from
/// flags, then env, then this profile — so connecting to a deployment is a
/// one-time act, not per-command environment configuration.
pub fn cloud_profile_path(home: &std::path::Path) -> PathBuf {
    home.join("cloud.json")
}

pub fn read_cloud_profile(home: &std::path::Path) -> Option<serde_json::Value> {
    let raw = std::fs::read_to_string(cloud_profile_path(home)).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn write_cloud_profile(home: &std::path::Path, profile: &serde_json::Value) -> Result<()> {
    std::fs::create_dir_all(home)?;
    let path = cloud_profile_path(home);
    std::fs::write(&path, format!("{:#}\n", profile))
        .map_err(|err| anyhow!("failed to write cloud profile {}: {err}", path.display()))
}

pub fn cloud_profile_string(home: &std::path::Path, pointer: &str) -> Option<String> {
    read_cloud_profile(home)?
        .pointer(pointer)?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}
