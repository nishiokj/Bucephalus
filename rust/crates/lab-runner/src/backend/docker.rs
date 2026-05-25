use anyhow::{anyhow, Context, Result};
use bytes::{Buf, Bytes, BytesMut};
use futures_util::stream::StreamExt;
use hyper::body::to_bytes;
use hyper::client::conn;
use hyper::{Body, Method, Request, Response, StatusCode};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::runtime::Runtime;
use tokio::time;

const DOCKER_API_VERSION: &str = "v1.43";
const DEFAULT_DOCKER_SOCKET_PATH: &str = "/var/run/docker.sock";
const IDLE_CONTAINER_COMMAND: &[&str] = &["/bin/sh", "-lc", "while true; do sleep 3600; done"];
const AGENTLAB_DOCKER_START_READY_TIMEOUT_MS_ENV: &str = "AGENTLAB_DOCKER_START_READY_TIMEOUT_MS";
const AGENTLAB_DOCKER_API_TIMEOUT_MS_ENV: &str = "AGENTLAB_DOCKER_API_TIMEOUT_MS";
const AGENTLAB_DOCKER_MAX_IMAGE_PULLS_ENV: &str = "AGENTLAB_DOCKER_MAX_IMAGE_PULLS";
const AGENTLAB_DOCKER_MAX_CONTAINER_STARTS_ENV: &str = "AGENTLAB_DOCKER_MAX_CONTAINER_STARTS";
const AGENTLAB_DOCKER_CLEANUP_RETRIES_ENV: &str = "AGENTLAB_DOCKER_CLEANUP_RETRIES";
const DEFAULT_DOCKER_START_READY_TIMEOUT_MS: u64 = 10_000;
const DEFAULT_DOCKER_API_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_DOCKER_MAX_IMAGE_PULLS: usize = 1;
const DEFAULT_DOCKER_MAX_CONTAINER_STARTS: usize = 2;
const DEFAULT_DOCKER_CLEANUP_RETRIES: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ImageKey {
    image_ref: String,
    platform: Option<String>,
}

struct DockerCoordinator {
    image_locks: Mutex<BTreeMap<ImageKey, Arc<Mutex<()>>>>,
    image_pull_limiter: CountingSemaphore,
    container_start_limiter: CountingSemaphore,
}

struct CountingSemaphore {
    state: Mutex<usize>,
    cv: Condvar,
    max: usize,
}

struct CountingSemaphorePermit<'a> {
    semaphore: &'a CountingSemaphore,
}

struct DockerImageLockCleanup<'a> {
    coordinator: &'a DockerCoordinator,
    key: &'a ImageKey,
    lock: &'a Arc<Mutex<()>>,
}

impl CountingSemaphore {
    fn new(max: usize) -> Self {
        Self {
            state: Mutex::new(0),
            cv: Condvar::new(),
            max: max.max(1),
        }
    }

    fn acquire(&self) -> CountingSemaphorePermit<'_> {
        let mut in_flight = self.state.lock().expect("docker limiter lock poisoned");
        while *in_flight >= self.max {
            in_flight = self
                .cv
                .wait(in_flight)
                .expect("docker limiter lock poisoned");
        }
        *in_flight += 1;
        CountingSemaphorePermit { semaphore: self }
    }
}

impl Drop for CountingSemaphorePermit<'_> {
    fn drop(&mut self) {
        let mut in_flight = self
            .semaphore
            .state
            .lock()
            .expect("docker limiter lock poisoned");
        *in_flight = in_flight.saturating_sub(1);
        self.semaphore.cv.notify_one();
    }
}

impl Drop for DockerImageLockCleanup<'_> {
    fn drop(&mut self) {
        self.coordinator.cleanup_image_lock(self.key, self.lock);
    }
}

impl DockerCoordinator {
    fn new() -> Self {
        Self {
            image_locks: Mutex::new(BTreeMap::new()),
            image_pull_limiter: CountingSemaphore::new(resolve_max_image_pulls()),
            container_start_limiter: CountingSemaphore::new(resolve_max_container_starts()),
        }
    }

    fn image_key(image_ref: &str, platform: Option<&str>) -> ImageKey {
        ImageKey {
            image_ref: image_ref.to_string(),
            platform: platform
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        }
    }

    fn image_lock(&self, key: &ImageKey) -> Arc<Mutex<()>> {
        let mut locks = self.image_locks.lock().expect("docker image lock poisoned");
        locks
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn cleanup_image_lock(&self, key: &ImageKey, lock: &Arc<Mutex<()>>) {
        let mut locks = self.image_locks.lock().expect("docker image lock poisoned");
        let should_remove = locks
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, lock) && Arc::strong_count(lock) == 2);
        if should_remove {
            locks.remove(key);
        }
    }

    fn ensure_image(
        &self,
        runtime: &DockerRuntime,
        image_ref: &str,
        platform: Option<&str>,
    ) -> Result<ImageMetadata> {
        let key = Self::image_key(image_ref, platform);
        let lock = self.image_lock(&key);
        let _single_flight = lock.lock().expect("docker image lock poisoned");
        let _cleanup = DockerImageLockCleanup {
            coordinator: self,
            key: &key,
            lock: &lock,
        };
        runtime.ensure_image_direct(image_ref, key.platform.as_deref())
    }
}

fn docker_coordinator() -> &'static DockerCoordinator {
    static COORDINATOR: OnceLock<DockerCoordinator> = OnceLock::new();
    COORDINATOR.get_or_init(DockerCoordinator::new)
}

#[cfg(test)]
pub(crate) fn docker_image_lock_for_test(
    image_ref: &str,
    platform: Option<&str>,
) -> Arc<Mutex<()>> {
    let key = DockerCoordinator::image_key(image_ref, platform);
    docker_coordinator().image_lock(&key)
}

#[cfg(test)]
pub(crate) fn docker_cleanup_image_lock_for_test(
    image_ref: &str,
    platform: Option<&str>,
    lock: &Arc<Mutex<()>>,
) {
    let key = DockerCoordinator::image_key(image_ref, platform);
    docker_coordinator().cleanup_image_lock(&key, lock);
}

#[cfg(test)]
pub(crate) fn docker_image_lock_exists_for_test(image_ref: &str, platform: Option<&str>) -> bool {
    let key = DockerCoordinator::image_key(image_ref, platform);
    docker_coordinator()
        .image_locks
        .lock()
        .expect("docker image lock poisoned")
        .contains_key(&key)
}

#[derive(Debug, Clone)]
pub(crate) struct ImageMetadata {
    pub(crate) image_id: Option<String>,
    pub(crate) repo_digests: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ContainerMount {
    pub(crate) host_path: PathBuf,
    pub(crate) container_path: String,
    pub(crate) read_only: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ContainerSpec {
    pub(crate) image: String,
    pub(crate) name: Option<String>,
    pub(crate) labels: BTreeMap<String, String>,
    pub(crate) platform: Option<String>,
    pub(crate) command: Vec<String>,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) workdir: Option<String>,
    pub(crate) mounts: Vec<ContainerMount>,
    pub(crate) tmpfs: BTreeMap<String, String>,
    pub(crate) network_mode: Option<String>,
    pub(crate) network_aliases: Vec<String>,
    pub(crate) security_opt: Vec<String>,
    pub(crate) cap_drop: Vec<String>,
    pub(crate) cpu_count: Option<u64>,
    pub(crate) memory_mb: Option<u64>,
}

impl ContainerSpec {
    pub(crate) fn idle(image: impl Into<String>) -> Self {
        Self {
            image: image.into(),
            name: None,
            labels: BTreeMap::new(),
            platform: None,
            command: IDLE_CONTAINER_COMMAND
                .iter()
                .map(|v| v.to_string())
                .collect(),
            env: BTreeMap::new(),
            workdir: None,
            mounts: Vec::new(),
            tmpfs: BTreeMap::new(),
            network_mode: None,
            network_aliases: Vec::new(),
            security_opt: Vec::new(),
            cap_drop: Vec::new(),
            cpu_count: None,
            memory_mb: None,
        }
    }

    pub(crate) fn image_default(image: impl Into<String>) -> Self {
        let mut spec = Self::idle(image);
        spec.command.clear();
        spec
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ContainerHandle {
    pub(crate) container_id: String,
}

#[derive(Debug, Clone)]
pub(crate) struct NetworkHandle {
    pub(crate) network_id: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ExecSpec {
    pub(crate) command: Vec<String>,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) workdir: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ExecHandle {
    pub(crate) exec_id: String,
    pub(crate) container_id: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ExecStatus {
    pub(crate) exit_code: Option<i32>,
}

#[derive(Debug, Clone)]
pub(crate) struct StreamExecResult {
    pub(crate) timed_out: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ContainerState {
    pub(crate) running: bool,
    pub(crate) status: Option<String>,
    pub(crate) exit_code: Option<i32>,
}

pub(crate) struct DockerRuntime {
    socket_path: PathBuf,
    runtime: Runtime,
}

impl DockerRuntime {
    pub(crate) fn connect() -> Result<Self> {
        let socket_path = docker_socket_path()?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("failed to build tokio runtime for docker backend")?;
        Ok(Self {
            socket_path,
            runtime,
        })
    }

    pub(crate) fn ping(&self) -> Result<()> {
        self.runtime.block_on(self.ping_async())
    }

    pub(crate) fn ensure_image(&self, image_ref: &str) -> Result<ImageMetadata> {
        self.ensure_image_with_platform(image_ref, None)
    }

    pub(crate) fn ensure_image_with_platform(
        &self,
        image_ref: &str,
        platform: Option<&str>,
    ) -> Result<ImageMetadata> {
        docker_coordinator().ensure_image(self, image_ref, platform)
    }

    fn ensure_image_direct(
        &self,
        image_ref: &str,
        platform: Option<&str>,
    ) -> Result<ImageMetadata> {
        self.runtime
            .block_on(self.ensure_image_direct_async(image_ref, platform))
    }

    pub(crate) fn create_and_start_container_checked(
        &self,
        spec: &ContainerSpec,
        context: &str,
    ) -> Result<ContainerHandle> {
        self.runtime
            .block_on(self.create_and_start_container_checked_async(spec, context))
    }

    pub(crate) fn create_network(
        &self,
        name: &str,
        internal: bool,
        labels: BTreeMap<String, String>,
    ) -> Result<()> {
        self.runtime
            .block_on(self.create_network_async(name, internal, labels))
    }

    pub(crate) fn remove_network(&self, name: &str) -> Result<()> {
        self.runtime.block_on(self.remove_network_async(name))
    }

    pub(crate) fn probe_image_idle_command(&self, image: &str) -> Result<()> {
        let mut spec = ContainerSpec::idle(image.to_string());
        spec.labels
            .insert("agentlab.role".to_string(), "preflight".to_string());
        spec.network_mode = Some("none".to_string());
        let handle =
            self.create_and_start_container_checked(&spec, "preflight idle container probe")?;
        let remove_result = self.remove_container_with_retry(
            &handle,
            true,
            "preflight idle container probe cleanup",
        );
        remove_result?;
        Ok(())
    }

    pub(crate) fn exec(&self, handle: &ContainerHandle, spec: &ExecSpec) -> Result<ExecHandle> {
        self.runtime.block_on(self.exec_async(handle, spec))
    }

    pub(crate) fn stream_exec_output(
        &self,
        handle: &ExecHandle,
        stdout_path: &Path,
        stderr_path: &Path,
        timeout: Option<Duration>,
    ) -> Result<StreamExecResult> {
        self.runtime.block_on(self.stream_exec_output_async(
            handle,
            stdout_path,
            stderr_path,
            timeout,
        ))
    }

    pub(crate) fn wait_exec(&self, handle: &ExecHandle) -> Result<ExecStatus> {
        self.runtime.block_on(self.wait_exec_async(handle))
    }

    pub(crate) fn list_containers_by_labels(
        &self,
        labels: &[String],
    ) -> Result<Vec<ContainerHandle>> {
        self.runtime
            .block_on(self.list_containers_by_labels_async(labels, true))
    }

    pub(crate) fn list_running_containers_by_labels(
        &self,
        labels: &[String],
    ) -> Result<Vec<ContainerHandle>> {
        self.runtime
            .block_on(self.list_containers_by_labels_async(labels, false))
    }

    pub(crate) fn list_networks_by_labels(&self, labels: &[String]) -> Result<Vec<NetworkHandle>> {
        self.runtime
            .block_on(self.list_networks_by_labels_async(labels))
    }

    #[cfg(test)]
    pub(crate) fn inspect_container(&self, handle: &ContainerHandle) -> Result<ContainerState> {
        self.runtime.block_on(self.inspect_container_async(handle))
    }

    #[cfg(test)]
    pub(crate) fn remove_container(&self, handle: &ContainerHandle, force: bool) -> Result<()> {
        self.runtime
            .block_on(self.remove_container_async(handle, force))
    }

    pub(crate) fn remove_container_with_retry(
        &self,
        handle: &ContainerHandle,
        force: bool,
        context: &str,
    ) -> Result<()> {
        self.runtime
            .block_on(self.remove_container_with_retry_async(handle, force, context))
    }

    pub(crate) fn pause_container(&self, handle: &ContainerHandle) -> Result<()> {
        self.runtime.block_on(self.pause_container_async(handle))
    }

    pub(crate) fn unpause_container(&self, handle: &ContainerHandle) -> Result<()> {
        self.runtime.block_on(self.unpause_container_async(handle))
    }

    pub(crate) fn kill_container(&self, handle: &ContainerHandle) -> Result<()> {
        self.runtime.block_on(self.kill_container_async(handle))
    }

    async fn ping_async(&self) -> Result<()> {
        let response = self
            .send_request(Method::GET, "/_ping", Body::empty(), None)
            .await?;
        expect_status(response.status(), &[StatusCode::OK], "docker ping")?;
        Ok(())
    }

    async fn ensure_image_direct_async(
        &self,
        image_ref: &str,
        platform: Option<&str>,
    ) -> Result<ImageMetadata> {
        match self.inspect_image_async(image_ref).await {
            Ok(metadata) => return Ok(metadata),
            Err(err) if is_not_found_error(&err) => {}
            Err(err) => return Err(err),
        }

        let _pull_permit = docker_coordinator().image_pull_limiter.acquire();
        self.pull_image_async(image_ref, platform).await?;
        self.inspect_image_async(image_ref).await
    }

    async fn inspect_image_async(&self, image_ref: &str) -> Result<ImageMetadata> {
        let response = self
            .send_request(
                Method::GET,
                &format!("/images/{}/json", encode_component(image_ref)),
                Body::empty(),
                None,
            )
            .await?;
        match response.status() {
            StatusCode::OK => {}
            StatusCode::NOT_FOUND => {
                return Err(anyhow!("docker image not found: {}", image_ref));
            }
            other => {
                return Err(anyhow!(
                    "docker image inspect failed for {}: {}",
                    image_ref,
                    response_error_text(response).await?
                )
                .context(format!("unexpected status {}", other.as_u16())));
            }
        }
        let payload: InspectImageResponse =
            serde_json::from_slice(&read_body_bytes(response, "docker image inspect body").await?)?;
        Ok(ImageMetadata {
            image_id: payload.id,
            repo_digests: payload.repo_digests.unwrap_or_default(),
        })
    }

    async fn pull_image_async(&self, image_ref: &str, platform: Option<&str>) -> Result<()> {
        let mut query = format!("fromImage={}", encode_query_value(image_ref));
        if let Some(platform) = platform.map(str::trim).filter(|value| !value.is_empty()) {
            query.push_str("&platform=");
            query.push_str(&encode_query_value(platform));
        }
        let response = self
            .send_request(
                Method::POST,
                &format!("/images/create?{}", query),
                Body::empty(),
                None,
            )
            .await?;
        expect_status(
            response.status(),
            &[StatusCode::OK, StatusCode::CREATED],
            "docker image pull",
        )?;
        let _ = to_bytes(response.into_body()).await?;
        Ok(())
    }

    async fn list_containers_by_labels_async(
        &self,
        labels: &[String],
        all: bool,
    ) -> Result<Vec<ContainerHandle>> {
        let filters = json!({ "label": labels });
        let response = self
            .send_request(
                Method::GET,
                &format!(
                    "/containers/json?all={}&filters={}",
                    if all { "1" } else { "0" },
                    encode_query_value(&filters.to_string())
                ),
                Body::empty(),
                None,
            )
            .await?;
        expect_status(
            response.status(),
            &[StatusCode::OK],
            "docker list containers by label",
        )?;
        let payload: Vec<ListContainerResponse> = serde_json::from_slice(
            &read_body_bytes(response, "docker list containers body").await?,
        )?;
        Ok(payload
            .into_iter()
            .filter_map(|container| container.id)
            .map(|container_id| ContainerHandle { container_id })
            .collect())
    }

    async fn list_networks_by_labels_async(&self, labels: &[String]) -> Result<Vec<NetworkHandle>> {
        let filters = json!({ "label": labels });
        let response = self
            .send_request(
                Method::GET,
                &format!(
                    "/networks?filters={}",
                    encode_query_value(&filters.to_string())
                ),
                Body::empty(),
                None,
            )
            .await?;
        expect_status(response.status(), &[StatusCode::OK], "docker list networks")?;
        let payload: Vec<ListNetworkResponse> =
            serde_json::from_slice(&read_body_bytes(response, "docker list networks body").await?)?;
        Ok(payload
            .into_iter()
            .filter_map(|network| {
                network
                    .id
                    .or(network.name)
                    .map(|network_id| NetworkHandle { network_id })
            })
            .collect())
    }

    async fn create_container_async(&self, spec: &ContainerSpec) -> Result<ContainerHandle> {
        let binds = spec
            .mounts
            .iter()
            .map(|mount| {
                format!(
                    "{}:{}{}",
                    mount.host_path.display(),
                    mount.container_path,
                    if mount.read_only { ":ro" } else { "" }
                )
            })
            .collect::<Vec<_>>();
        let env = spec
            .env
            .iter()
            .map(|(key, value)| format!("{}={}", key, value))
            .collect::<Vec<_>>();
        let host_config = json!({
            "Binds": binds,
            "Tmpfs": spec.tmpfs,
            "NetworkMode": spec.network_mode.clone().unwrap_or_else(|| "default".to_string()),
            "SecurityOpt": spec.security_opt,
            "CapDrop": spec.cap_drop,
            "NanoCpus": spec.cpu_count.map(|count| count.saturating_mul(1_000_000_000u64)),
            "Memory": spec.memory_mb.map(|mb| mb.saturating_mul(1024 * 1024)),
        });
        let mut payload = json!({
            "Image": spec.image,
            "Env": env,
            "Labels": spec.labels,
            "WorkingDir": spec.workdir,
            "AttachStdout": false,
            "AttachStderr": false,
            "Tty": false,
            "HostConfig": host_config,
        });
        if !spec.command.is_empty() {
            payload["Cmd"] = json!(spec.command);
        }
        if !spec.network_aliases.is_empty() {
            if let Some(network_name) = spec.network_mode.as_deref().filter(|value| {
                !value.is_empty() && *value != "none" && *value != "host" && *value != "default"
            }) {
                payload["NetworkingConfig"] = json!({
                    "EndpointsConfig": {
                        network_name: {
                            "Aliases": spec.network_aliases,
                        }
                    }
                });
            }
        }
        let mut query_parts = Vec::new();
        if let Some(name) = spec.name.as_deref().filter(|value| !value.is_empty()) {
            query_parts.push(format!("name={}", encode_query_value(name)));
        }
        if let Some(platform) = spec.platform.as_deref().filter(|value| !value.is_empty()) {
            query_parts.push(format!("platform={}", encode_query_value(platform)));
        }
        let mut path = "/containers/create".to_string();
        if !query_parts.is_empty() {
            path.push('?');
            path.push_str(&query_parts.join("&"));
        }
        let response = self
            .send_request(
                Method::POST,
                &path,
                Body::from(serde_json::to_vec(&payload)?),
                Some("application/json"),
            )
            .await?;
        expect_status(
            response.status(),
            &[StatusCode::CREATED],
            "docker create container",
        )?;
        let payload: CreateContainerResponse = serde_json::from_slice(
            &read_body_bytes(response, "docker create container body").await?,
        )?;
        Ok(ContainerHandle {
            container_id: payload.id,
        })
    }

    async fn create_and_start_container_checked_async(
        &self,
        spec: &ContainerSpec,
        context: &str,
    ) -> Result<ContainerHandle> {
        let _start_permit = docker_coordinator().container_start_limiter.acquire();
        let handle = self.create_container_async(spec).await?;
        let start_result = async {
            self.start_container_async(&handle).await?;
            self.wait_container_running_async(&handle, spec, context)
                .await
        }
        .await;
        if let Err(err) = start_result {
            let cleanup_result = self
                .remove_container_with_retry_async(&handle, true, "container start failure cleanup")
                .await;
            if let Err(cleanup_err) = cleanup_result {
                return Err(err.context(format!(
                    "also failed to remove container {} after start failure: {}",
                    handle.container_id, cleanup_err
                )));
            }
            return Err(err);
        }
        Ok(handle)
    }

    async fn create_network_async(
        &self,
        name: &str,
        internal: bool,
        labels: BTreeMap<String, String>,
    ) -> Result<()> {
        let payload = json!({
            "Name": name,
            "Driver": "bridge",
            "Internal": internal,
            "Labels": labels,
        });
        let response = self
            .send_request(
                Method::POST,
                "/networks/create",
                Body::from(serde_json::to_vec(&payload)?),
                Some("application/json"),
            )
            .await?;
        expect_status(
            response.status(),
            &[StatusCode::CREATED],
            "docker create network",
        )?;
        let _ = read_body_bytes(response, "docker create network body").await?;
        Ok(())
    }

    async fn remove_network_async(&self, name: &str) -> Result<()> {
        let response = self
            .send_request(
                Method::DELETE,
                &format!("/networks/{}", encode_component(name)),
                Body::empty(),
                None,
            )
            .await?;
        expect_status(
            response.status(),
            &[StatusCode::NO_CONTENT],
            "docker remove network",
        )?;
        Ok(())
    }

    async fn start_container_async(&self, handle: &ContainerHandle) -> Result<()> {
        let response = self
            .send_request(
                Method::POST,
                &format!("/containers/{}/start", handle.container_id),
                Body::empty(),
                None,
            )
            .await?;
        expect_status(
            response.status(),
            &[StatusCode::NO_CONTENT, StatusCode::NOT_MODIFIED],
            "docker start container",
        )?;
        Ok(())
    }

    async fn remove_container_with_retry_async(
        &self,
        handle: &ContainerHandle,
        force: bool,
        context: &str,
    ) -> Result<()> {
        let attempts = docker_cleanup_retries().saturating_add(1);
        let mut last_error = None;
        for attempt in 1..=attempts {
            match self.remove_container_async(handle, force).await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    last_error = Some(err);
                    if attempt < attempts {
                        time::sleep(Duration::from_millis(100 * attempt as u64)).await;
                    }
                }
            }
        }
        Err(last_error
            .unwrap_or_else(|| anyhow!("docker remove container failed without error"))
            .context(format!(
                "{} failed after {} attempt(s): container={}",
                context, attempts, handle.container_id
            )))
    }

    async fn wait_container_running_async(
        &self,
        handle: &ContainerHandle,
        spec: &ContainerSpec,
        context: &str,
    ) -> Result<()> {
        let timeout = docker_start_ready_timeout();
        let deadline = time::Instant::now() + timeout;
        loop {
            let state = self.inspect_container_async(handle).await?;
            if state.running {
                return Ok(());
            }
            let status = state.status.as_deref().unwrap_or("unknown");
            if matches!(status, "exited" | "dead" | "removing") || state.exit_code.is_some() {
                return Err(container_not_running_error(context, handle, spec, &state));
            }
            if time::Instant::now() >= deadline {
                return Err(anyhow!(
                    "{} did not become running within {}ms: container={} image={} status={} exit_code={}",
                    context,
                    timeout.as_millis(),
                    handle.container_id,
                    spec.image,
                    state.status.as_deref().unwrap_or("unknown"),
                    format_exit_code(state.exit_code)
                ));
            }
            time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn exec_async(&self, handle: &ContainerHandle, spec: &ExecSpec) -> Result<ExecHandle> {
        let env = spec
            .env
            .iter()
            .map(|(key, value)| format!("{}={}", key, value))
            .collect::<Vec<_>>();
        let payload = json!({
            "AttachStdout": true,
            "AttachStderr": true,
            "Cmd": spec.command,
            "Env": env,
            "WorkingDir": spec.workdir,
            "Tty": false,
        });
        let response = self
            .send_request(
                Method::POST,
                &format!("/containers/{}/exec", handle.container_id),
                Body::from(serde_json::to_vec(&payload)?),
                Some("application/json"),
            )
            .await?;
        expect_status(
            response.status(),
            &[StatusCode::CREATED],
            "docker create exec",
        )?;
        let payload: CreateExecResponse =
            serde_json::from_slice(&read_body_bytes(response, "docker create exec body").await?)?;
        Ok(ExecHandle {
            exec_id: payload.id,
            container_id: handle.container_id.clone(),
        })
    }

    async fn stream_exec_output_async(
        &self,
        handle: &ExecHandle,
        stdout_path: &Path,
        stderr_path: &Path,
        timeout: Option<Duration>,
    ) -> Result<StreamExecResult> {
        ensure_parent_dir(stdout_path)?;
        ensure_parent_dir(stderr_path)?;
        let payload = json!({
            "Detach": false,
            "Tty": false,
        });
        let response = self
            .send_request(
                Method::POST,
                &format!("/exec/{}/start", handle.exec_id),
                Body::from(serde_json::to_vec(&payload)?),
                Some("application/json"),
            )
            .await?;
        expect_status(
            response.status(),
            &[StatusCode::OK, StatusCode::CREATED],
            "docker start exec",
        )?;

        let mut stdout_file = fs::File::create(stdout_path)?;
        let mut stderr_file = fs::File::create(stderr_path)?;
        let body = response.into_body();

        let stream_future = async {
            let mut body_stream = body;
            let mut pending = BytesMut::new();
            while let Some(chunk) = body_stream.next().await {
                let chunk = chunk?;
                pending.extend_from_slice(&chunk);
                drain_multiplexed_frames(&mut pending, &mut stdout_file, &mut stderr_file)?;
            }
            if !pending.is_empty() {
                stdout_file.write_all(&pending)?;
                pending.clear();
            }
            stdout_file.flush()?;
            stderr_file.flush()?;
            Ok::<(), anyhow::Error>(())
        };

        let timed_out = if let Some(timeout) = timeout {
            match time::timeout(timeout, stream_future).await {
                Ok(result) => {
                    result?;
                    false
                }
                Err(_) => {
                    self.kill_container_async(&ContainerHandle {
                        container_id: handle.container_id.clone(),
                    })
                    .await?;
                    true
                }
            }
        } else {
            stream_future.await?;
            false
        };

        Ok(StreamExecResult { timed_out })
    }

    async fn wait_exec_async(&self, handle: &ExecHandle) -> Result<ExecStatus> {
        let deadline = time::Instant::now() + Duration::from_secs(5);
        loop {
            let response = self
                .send_request(
                    Method::GET,
                    &format!("/exec/{}/json", handle.exec_id),
                    Body::empty(),
                    None,
                )
                .await?;
            expect_status(response.status(), &[StatusCode::OK], "docker inspect exec")?;
            let payload: InspectExecResponse = serde_json::from_slice(
                &read_body_bytes(response, "docker inspect exec body").await?,
            )?;
            if !payload.running || time::Instant::now() >= deadline {
                return Ok(ExecStatus {
                    exit_code: payload.exit_code,
                });
            }
            time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn inspect_container_async(&self, handle: &ContainerHandle) -> Result<ContainerState> {
        let response = self
            .send_request(
                Method::GET,
                &format!("/containers/{}/json", handle.container_id),
                Body::empty(),
                None,
            )
            .await?;
        expect_status(
            response.status(),
            &[StatusCode::OK],
            "docker inspect container",
        )?;
        let payload: InspectContainerResponse = serde_json::from_slice(
            &read_body_bytes(response, "docker inspect container body").await?,
        )?;
        Ok(ContainerState {
            running: payload
                .state
                .as_ref()
                .and_then(|state| state.running)
                .unwrap_or(false),
            status: payload
                .state
                .as_ref()
                .and_then(|state| state.status.clone()),
            exit_code: payload.state.and_then(|state| state.exit_code),
        })
    }

    async fn remove_container_async(&self, handle: &ContainerHandle, force: bool) -> Result<()> {
        let response = self
            .send_request(
                Method::DELETE,
                &format!(
                    "/containers/{}?force={}",
                    handle.container_id,
                    if force { "1" } else { "0" }
                ),
                Body::empty(),
                None,
            )
            .await?;
        expect_status(
            response.status(),
            &[StatusCode::NO_CONTENT, StatusCode::NOT_FOUND],
            "docker remove container",
        )?;
        Ok(())
    }

    async fn pause_container_async(&self, handle: &ContainerHandle) -> Result<()> {
        let response = self
            .send_request(
                Method::POST,
                &format!("/containers/{}/pause", handle.container_id),
                Body::empty(),
                None,
            )
            .await?;
        expect_status(
            response.status(),
            &[
                StatusCode::NO_CONTENT,
                StatusCode::NOT_MODIFIED,
                StatusCode::CONFLICT,
                StatusCode::NOT_FOUND,
            ],
            "docker pause container",
        )?;
        Ok(())
    }

    async fn unpause_container_async(&self, handle: &ContainerHandle) -> Result<()> {
        let response = self
            .send_request(
                Method::POST,
                &format!("/containers/{}/unpause", handle.container_id),
                Body::empty(),
                None,
            )
            .await?;
        expect_status(
            response.status(),
            &[
                StatusCode::NO_CONTENT,
                StatusCode::NOT_MODIFIED,
                StatusCode::CONFLICT,
                StatusCode::NOT_FOUND,
            ],
            "docker unpause container",
        )?;
        Ok(())
    }

    async fn kill_container_async(&self, handle: &ContainerHandle) -> Result<()> {
        let response = self
            .send_request(
                Method::POST,
                &format!("/containers/{}/kill?signal=KILL", handle.container_id),
                Body::empty(),
                None,
            )
            .await?;
        expect_status(
            response.status(),
            &[
                StatusCode::NO_CONTENT,
                StatusCode::NOT_MODIFIED,
                StatusCode::NOT_FOUND,
            ],
            "docker kill container",
        )?;
        Ok(())
    }

    async fn send_request(
        &self,
        method: Method,
        path: &str,
        body: Body,
        content_type: Option<&str>,
    ) -> Result<Response<Body>> {
        let mut builder = Request::builder()
            .method(method)
            .uri(docker_uri(path))
            .header("Host", "docker");
        if let Some(content_type) = content_type {
            builder = builder.header("Content-Type", content_type);
        }
        let request = builder.body(body)?;
        let timeout_context = format!("docker API {} {}", request.method(), path);
        with_docker_api_timeout(&timeout_context, async {
            let stream = UnixStream::connect(&self.socket_path)
                .await
                .with_context(|| {
                    format!(
                        "failed to connect to docker socket {}",
                        self.socket_path.display()
                    )
                })?;
            let (mut sender, connection) = conn::handshake(stream).await?;
            tokio::spawn(async move {
                let _ = connection.await;
            });
            sender.send_request(request).await.map_err(Into::into)
        })
        .await
    }
}

pub(crate) fn resolve_image_digest(image: &str) -> Option<String> {
    let runtime = DockerRuntime::connect().ok()?;
    let metadata = runtime.ensure_image(image).ok()?;
    metadata
        .repo_digests
        .first()
        .and_then(|value| value.rsplit_once('@').map(|(_, digest)| digest.to_string()))
        .or(metadata.image_id)
}

fn docker_socket_path() -> Result<PathBuf> {
    let host = std::env::var("DOCKER_HOST").unwrap_or_default();
    if !host.is_empty() {
        return parse_unix_docker_host(&host);
    }

    let context = std::env::var("DOCKER_CONTEXT").unwrap_or_default();
    if !context.is_empty() {
        if let Some(path) = docker_socket_path_for_context(&context)? {
            return Ok(path);
        }
    }

    if let Some(path) = docker_socket_path_from_current_context()? {
        return Ok(path);
    }

    Ok(PathBuf::from(DEFAULT_DOCKER_SOCKET_PATH))
}

fn parse_unix_docker_host(host: &str) -> Result<PathBuf> {
    if let Some(path) = host.strip_prefix("unix://") {
        return Ok(PathBuf::from(path));
    }
    Err(anyhow!(
        "unsupported DOCKER_HOST '{}'; only unix:// sockets are supported",
        host
    ))
}

fn docker_socket_path_from_current_context() -> Result<Option<PathBuf>> {
    let config_path = match docker_config_path() {
        Some(path) => path,
        None => return Ok(None),
    };
    if !config_path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(&config_path)
        .with_context(|| format!("failed to read docker config {}", config_path.display()))?;
    let config: DockerConfig = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse docker config {}", config_path.display()))?;
    let Some(context_name) = config
        .current_context
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    docker_socket_path_for_context(&context_name)
}

fn docker_socket_path_for_context(context_name: &str) -> Result<Option<PathBuf>> {
    let meta_root = match docker_contexts_meta_root() {
        Some(path) => path,
        None => return Ok(None),
    };
    if !meta_root.exists() {
        return Ok(None);
    }

    for entry in fs::read_dir(&meta_root)
        .with_context(|| format!("failed to read docker contexts dir {}", meta_root.display()))?
    {
        let entry = entry?;
        let meta_path = entry.path().join("meta.json");
        if !meta_path.exists() {
            continue;
        }
        let raw = fs::read_to_string(&meta_path).with_context(|| {
            format!(
                "failed to read docker context metadata {}",
                meta_path.display()
            )
        })?;
        let meta: DockerContextMetadata = serde_json::from_str(&raw).with_context(|| {
            format!(
                "failed to parse docker context metadata {}",
                meta_path.display()
            )
        })?;
        if meta.name != context_name {
            continue;
        }
        let Some(host) = meta.endpoints.docker.and_then(|endpoint| endpoint.host) else {
            return Ok(None);
        };
        return Ok(Some(parse_unix_docker_host(&host)?));
    }

    Ok(None)
}

fn docker_config_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".docker").join("config.json"))
}

fn docker_contexts_meta_root() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".docker")
            .join("contexts")
            .join("meta")
    })
}

fn docker_uri(path: &str) -> String {
    format!(
        "http://docker/{}/{}",
        DOCKER_API_VERSION,
        path.trim_start_matches('/')
    )
}

fn encode_component(raw: &str) -> String {
    utf8_percent_encode(raw, NON_ALPHANUMERIC).to_string()
}

#[derive(Deserialize)]
struct DockerConfig {
    #[serde(rename = "currentContext")]
    current_context: Option<String>,
}

#[derive(Deserialize)]
struct DockerContextMetadata {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Endpoints")]
    endpoints: DockerContextEndpoints,
}

#[derive(Deserialize)]
struct DockerContextEndpoints {
    docker: Option<DockerContextEndpoint>,
}

#[derive(Deserialize)]
struct DockerContextEndpoint {
    #[serde(rename = "Host")]
    host: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("{}_{}_{}", prefix, std::process::id(), stamp));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write_file(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parents");
        }
        fs::write(path, body).expect("write file");
    }

    #[test]
    fn docker_socket_path_uses_docker_host_when_present() {
        let _guard = env_lock().lock().expect("env lock");
        let old_home = std::env::var_os("HOME");
        let old_host = std::env::var_os("DOCKER_HOST");
        let old_context = std::env::var_os("DOCKER_CONTEXT");
        let home = unique_temp_dir("docker_socket_host");

        std::env::set_var("HOME", &home);
        std::env::set_var("DOCKER_HOST", "unix:///tmp/test-docker.sock");
        std::env::remove_var("DOCKER_CONTEXT");

        let resolved = docker_socket_path().expect("socket path");
        assert_eq!(resolved, PathBuf::from("/tmp/test-docker.sock"));

        match old_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match old_host {
            Some(value) => std::env::set_var("DOCKER_HOST", value),
            None => std::env::remove_var("DOCKER_HOST"),
        }
        match old_context {
            Some(value) => std::env::set_var("DOCKER_CONTEXT", value),
            None => std::env::remove_var("DOCKER_CONTEXT"),
        }
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn docker_socket_path_uses_current_context_from_docker_config() {
        let _guard = env_lock().lock().expect("env lock");
        let old_home = std::env::var_os("HOME");
        let old_host = std::env::var_os("DOCKER_HOST");
        let old_context = std::env::var_os("DOCKER_CONTEXT");
        let home = unique_temp_dir("docker_socket_context");

        write_file(
            &home.join(".docker").join("config.json"),
            r#"{"currentContext":"orbstack"}"#,
        );
        write_file(
            &home
                .join(".docker")
                .join("contexts")
                .join("meta")
                .join("abc123")
                .join("meta.json"),
            r#"{"Name":"orbstack","Endpoints":{"docker":{"Host":"unix:///Users/test/.orbstack/run/docker.sock"}}}"#,
        );

        std::env::set_var("HOME", &home);
        std::env::remove_var("DOCKER_HOST");
        std::env::remove_var("DOCKER_CONTEXT");

        let resolved = docker_socket_path().expect("socket path");
        assert_eq!(
            resolved,
            PathBuf::from("/Users/test/.orbstack/run/docker.sock")
        );

        match old_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match old_host {
            Some(value) => std::env::set_var("DOCKER_HOST", value),
            None => std::env::remove_var("DOCKER_HOST"),
        }
        match old_context {
            Some(value) => std::env::set_var("DOCKER_CONTEXT", value),
            None => std::env::remove_var("DOCKER_CONTEXT"),
        }
        let _ = fs::remove_dir_all(home);
    }
}

fn encode_query_value(raw: &str) -> String {
    utf8_percent_encode(raw, NON_ALPHANUMERIC).to_string()
}

fn expect_status(status: StatusCode, allowed: &[StatusCode], action: &str) -> Result<()> {
    if allowed.iter().any(|candidate| *candidate == status) {
        return Ok(());
    }
    Err(anyhow!(
        "{} returned unexpected docker status {}",
        action,
        status.as_u16()
    ))
}

async fn response_error_text(response: Response<Body>) -> Result<String> {
    let status = response.status();
    let body = read_body_bytes(response, "docker error response body").await?;
    let text = String::from_utf8_lossy(&body).trim().to_string();
    if text.is_empty() {
        Ok(format!("status {}", status.as_u16()))
    } else {
        Ok(text)
    }
}

async fn read_body_bytes(response: Response<Body>, action: &str) -> Result<Bytes> {
    with_docker_api_timeout(action, async { Ok(to_bytes(response.into_body()).await?) }).await
}

async fn with_docker_api_timeout<T, F>(action: &str, future: F) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    let timeout = docker_api_timeout();
    match time::timeout(timeout, future).await {
        Ok(result) => result,
        Err(_) => Err(anyhow!(
            "docker_api_timeout: {} exceeded {}ms waiting for Docker API",
            action,
            timeout.as_millis()
        )),
    }
}

fn docker_start_ready_timeout() -> Duration {
    std::env::var(AGENTLAB_DOCKER_START_READY_TIMEOUT_MS_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_DOCKER_START_READY_TIMEOUT_MS))
}

fn docker_api_timeout() -> Duration {
    std::env::var(AGENTLAB_DOCKER_API_TIMEOUT_MS_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_DOCKER_API_TIMEOUT_MS))
}

fn resolve_max_image_pulls() -> usize {
    std::env::var(AGENTLAB_DOCKER_MAX_IMAGE_PULLS_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_DOCKER_MAX_IMAGE_PULLS)
}

fn resolve_max_container_starts() -> usize {
    std::env::var(AGENTLAB_DOCKER_MAX_CONTAINER_STARTS_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_DOCKER_MAX_CONTAINER_STARTS)
}

fn docker_cleanup_retries() -> usize {
    std::env::var(AGENTLAB_DOCKER_CLEANUP_RETRIES_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_DOCKER_CLEANUP_RETRIES)
}

fn format_exit_code(exit_code: Option<i32>) -> String {
    exit_code
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn container_not_running_error(
    context: &str,
    handle: &ContainerHandle,
    spec: &ContainerSpec,
    state: &ContainerState,
) -> anyhow::Error {
    anyhow!(
        "{} exited before it was ready: container={} image={} status={} exit_code={}",
        context,
        handle.container_id,
        spec.image,
        state.status.as_deref().unwrap_or("unknown"),
        format_exit_code(state.exit_code)
    )
}

fn is_not_found_error(err: &anyhow::Error) -> bool {
    err.to_string().contains("not found")
}

fn drain_multiplexed_frames(
    pending: &mut BytesMut,
    stdout_file: &mut fs::File,
    stderr_file: &mut fs::File,
) -> Result<()> {
    loop {
        if pending.len() < 8 {
            return Ok(());
        }
        let stream_type = pending[0];
        let size = u32::from_be_bytes([pending[4], pending[5], pending[6], pending[7]]) as usize;
        if pending.len() < 8 + size {
            return Ok(());
        }
        let payload = pending[8..8 + size].to_vec();
        match stream_type {
            1 => stdout_file.write_all(&payload)?,
            2 => stderr_file.write_all(&payload)?,
            _ => stdout_file.write_all(&payload)?,
        }
        pending.advance(8 + size);
    }
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    Ok(())
}

fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct CreateContainerResponse {
    #[serde(rename = "Id")]
    id: String,
}

#[derive(Debug, Deserialize)]
struct ListContainerResponse {
    #[serde(rename = "Id")]
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListNetworkResponse {
    #[serde(rename = "Id")]
    id: Option<String>,
    #[serde(rename = "Name")]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateExecResponse {
    #[serde(rename = "Id")]
    id: String,
}

#[derive(Debug, Deserialize)]
struct InspectImageResponse {
    #[serde(rename = "Id")]
    id: Option<String>,
    #[serde(rename = "RepoDigests")]
    repo_digests: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct InspectExecResponse {
    #[serde(rename = "Running")]
    running: bool,
    #[serde(rename = "ExitCode")]
    exit_code: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct InspectContainerResponse {
    #[serde(rename = "State")]
    state: Option<InspectContainerState>,
}

#[derive(Debug, Deserialize)]
struct InspectContainerState {
    #[serde(rename = "Running")]
    running: Option<bool>,
    #[serde(rename = "Status")]
    status: Option<String>,
    #[serde(rename = "ExitCode")]
    exit_code: Option<i32>,
}
