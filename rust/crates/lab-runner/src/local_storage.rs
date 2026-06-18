use anyhow::{anyhow, Result};
use std::path::PathBuf;

pub const BUCEPHALUS_DB_ENV: &str = "BUCEPHALUS_DB";
pub const ACCOUNT_SQLITE_FILE: &str = "bucephalus.sqlite";
pub use lab_core::{
    bucephalus_home, cloud_profile_path, cloud_profile_string, read_cloud_profile,
    write_cloud_profile, BUCEPHALUS_HOME_ENV,
};

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
