use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Condvar, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ImageReferenceSource {
    OciRegistry,
    DockerDaemon,
    OciLayout,
    DockerArchive,
    RemoteObject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ImageReference {
    raw: String,
    pub(crate) source: ImageReferenceSource,
}

impl ImageReference {
    pub(crate) fn parse(raw: impl Into<String>) -> Result<Self> {
        let raw = raw.into();
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("image reference must not be empty"));
        }
        let source = if trimmed.starts_with("oci-layout://") {
            ImageReferenceSource::OciLayout
        } else if trimmed.starts_with("docker-archive://") {
            ImageReferenceSource::DockerArchive
        } else if trimmed.starts_with("docker-daemon://") {
            ImageReferenceSource::DockerDaemon
        } else if trimmed.starts_with("s3://")
            || trimmed.starts_with("gs://")
            || trimmed.starts_with("az://")
        {
            ImageReferenceSource::RemoteObject
        } else if trimmed.contains("://") {
            return Err(anyhow!("unsupported image reference scheme: {}", trimmed));
        } else {
            ImageReferenceSource::OciRegistry
        };
        Ok(Self {
            raw: trimmed.to_string(),
            source,
        })
    }

    pub(crate) fn raw(&self) -> &str {
        &self.raw
    }

    pub(crate) fn as_oci_registry_reference(&self) -> Result<OciRegistryReference> {
        OciRegistryReference::parse(self)
    }
}

impl fmt::Display for ImageReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum OciRegistryReferenceKind {
    Tag(String),
    Digest(String),
    TagAndDigest { tag: String, digest: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OciRegistryReference {
    pub(crate) registry: String,
    pub(crate) repository: String,
    pub(crate) kind: OciRegistryReferenceKind,
}

impl OciRegistryReference {
    pub(crate) fn parse(reference: &ImageReference) -> Result<Self> {
        if reference.source != ImageReferenceSource::OciRegistry {
            return Err(anyhow!(
                "image reference '{}' is not an OCI registry reference",
                reference.raw()
            ));
        }
        parse_oci_registry_reference(reference.raw())
    }
}

fn parse_oci_registry_reference(raw: &str) -> Result<OciRegistryReference> {
    let (name_part, digest) = match raw.split_once('@') {
        Some((name, digest)) => {
            validate_digest(digest)?;
            (name, Some(digest.to_string()))
        }
        None => (raw, None),
    };
    if name_part.is_empty() {
        return Err(anyhow!(
            "OCI registry image reference missing repository: {}",
            raw
        ));
    }

    let last_slash = name_part.rfind('/');
    let last_colon = name_part.rfind(':');
    let tag_colon = last_colon.filter(|colon| last_slash.is_none_or(|slash| *colon > slash));
    let (name_without_tag, tag) = if let Some(colon) = tag_colon {
        let tag = &name_part[colon + 1..];
        if tag.is_empty() {
            return Err(anyhow!(
                "OCI registry image reference has empty tag: {}",
                raw
            ));
        }
        (&name_part[..colon], Some(tag.to_string()))
    } else {
        (name_part, None)
    };
    if name_without_tag.is_empty() {
        return Err(anyhow!(
            "OCI registry image reference missing repository: {}",
            raw
        ));
    }

    let mut components = name_without_tag.split('/').collect::<Vec<_>>();
    if components.iter().any(|component| component.is_empty()) {
        return Err(anyhow!(
            "OCI registry image reference has empty path component: {}",
            raw
        ));
    }
    let first = components[0];
    let has_explicit_registry =
        first.contains('.') || first.contains(':') || first.eq_ignore_ascii_case("localhost");
    let registry = if has_explicit_registry {
        components.remove(0).to_string()
    } else {
        "docker.io".to_string()
    };
    if components.is_empty() {
        return Err(anyhow!(
            "OCI registry image reference missing repository: {}",
            raw
        ));
    }
    let repository = if !has_explicit_registry && components.len() == 1 {
        format!("library/{}", components[0])
    } else {
        components.join("/")
    };
    validate_repository(&repository, raw)?;

    let kind = match (tag, digest) {
        (Some(tag), Some(digest)) => OciRegistryReferenceKind::TagAndDigest { tag, digest },
        (Some(tag), None) => OciRegistryReferenceKind::Tag(tag),
        (None, Some(digest)) => OciRegistryReferenceKind::Digest(digest),
        (None, None) => OciRegistryReferenceKind::Tag("latest".to_string()),
    };
    Ok(OciRegistryReference {
        registry,
        repository,
        kind,
    })
}

fn validate_repository(repository: &str, raw: &str) -> Result<()> {
    if repository.split('/').any(|component| {
        component.is_empty() || component.starts_with('-') || component.ends_with('-')
    }) {
        return Err(anyhow!(
            "OCI registry image reference has invalid repository path: {}",
            raw
        ));
    }
    Ok(())
}

fn validate_digest(digest: &str) -> Result<()> {
    let Some((algorithm, encoded)) = digest.split_once(':') else {
        return Err(anyhow!("OCI image digest must be '<algorithm>:<encoded>'"));
    };
    if algorithm.is_empty()
        || encoded.is_empty()
        || !algorithm
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'))
        || !encoded
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-' | '='))
    {
        return Err(anyhow!("OCI image digest is invalid: {}", digest));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ImageRequirementRole {
    TaskSandbox,
    AgentRuntime,
    Grader,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ImageRequirement {
    pub(crate) role: ImageRequirementRole,
    pub(crate) image: ImageReference,
    pub(crate) platform: Option<String>,
}

impl ImageRequirement {
    pub(crate) fn new(
        role: ImageRequirementRole,
        raw: impl Into<String>,
        platform: Option<String>,
    ) -> Result<Self> {
        Ok(Self {
            role,
            image: ImageReference::parse(raw)?,
            platform: platform
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ImageResolutionMode {
    ReferenceOnly,
    Manifest,
    ManifestAndBlobHeads,
    Materialize,
    Smoke,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ImageResolveRequest {
    pub(crate) requirement: ImageRequirement,
    pub(crate) mode: ImageResolutionMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ImageResolveReport {
    pub(crate) requirement: ImageRequirement,
    pub(crate) resolved_digest: Option<String>,
    pub(crate) platform: Option<String>,
    pub(crate) manifest_size_bytes: Option<u64>,
    pub(crate) materialized: bool,
}

pub(crate) trait ImageResolver: Sync {
    fn supports(&self, _request: &ImageResolveRequest) -> bool {
        true
    }

    fn resolve(&self, request: &ImageResolveRequest) -> Result<ImageResolveReport>;
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ImageResolveCacheKey {
    source: ImageReferenceSource,
    raw: String,
    platform: Option<String>,
    mode: ImageResolutionMode,
}

impl ImageResolveCacheKey {
    pub(crate) fn from_request(request: &ImageResolveRequest) -> Self {
        Self {
            source: request.requirement.image.source,
            raw: request.requirement.image.raw().to_string(),
            platform: request.requirement.platform.clone(),
            mode: request.mode,
        }
    }
}

#[derive(Debug, Clone)]
enum ScopedImageCacheEntry {
    InFlight,
    Ready(ImageResolveReport),
}

pub(crate) struct ScopedImageResolverCache<'a> {
    inner: &'a dyn ImageResolver,
    state: Mutex<BTreeMap<ImageResolveCacheKey, ScopedImageCacheEntry>>,
    ready: Condvar,
}

impl<'a> ScopedImageResolverCache<'a> {
    pub(crate) fn new(inner: &'a dyn ImageResolver) -> Self {
        Self {
            inner,
            state: Mutex::new(BTreeMap::new()),
            ready: Condvar::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn cache_len(&self) -> usize {
        self.state
            .lock()
            .expect("image resolver cache lock poisoned")
            .len()
    }
}

struct ScopedImageInFlightCleanup<'a> {
    key: Option<ImageResolveCacheKey>,
    state: &'a Mutex<BTreeMap<ImageResolveCacheKey, ScopedImageCacheEntry>>,
    ready: &'a Condvar,
}

impl ScopedImageInFlightCleanup<'_> {
    fn disarm(&mut self) {
        self.key = None;
    }
}

impl Drop for ScopedImageInFlightCleanup<'_> {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            let mut state = self
                .state
                .lock()
                .expect("image resolver cache lock poisoned");
            state.remove(&key);
            self.ready.notify_all();
        }
    }
}

impl ImageResolver for ScopedImageResolverCache<'_> {
    fn supports(&self, request: &ImageResolveRequest) -> bool {
        self.inner.supports(request)
    }

    fn resolve(&self, request: &ImageResolveRequest) -> Result<ImageResolveReport> {
        let key = ImageResolveCacheKey::from_request(request);
        let mut state = self
            .state
            .lock()
            .expect("image resolver cache lock poisoned");
        loop {
            match state.get(&key) {
                Some(ScopedImageCacheEntry::Ready(report)) => return Ok(report.clone()),
                Some(ScopedImageCacheEntry::InFlight) => {
                    state = self
                        .ready
                        .wait(state)
                        .expect("image resolver cache lock poisoned");
                }
                None => {
                    state.insert(key.clone(), ScopedImageCacheEntry::InFlight);
                    break;
                }
            }
        }
        drop(state);

        let mut cleanup = ScopedImageInFlightCleanup {
            key: Some(key.clone()),
            state: &self.state,
            ready: &self.ready,
        };
        let result = self.inner.resolve(request);
        let mut state = self
            .state
            .lock()
            .expect("image resolver cache lock poisoned");
        match &result {
            Ok(report) => {
                state.insert(key, ScopedImageCacheEntry::Ready(report.clone()));
            }
            Err(_) => {
                state.remove(&key);
            }
        }
        cleanup.disarm();
        self.ready.notify_all();
        result
    }
}

pub(crate) struct ImageResolverChain<'a> {
    resolvers: Vec<&'a dyn ImageResolver>,
}

impl<'a> ImageResolverChain<'a> {
    pub(crate) fn new(resolvers: Vec<&'a dyn ImageResolver>) -> Self {
        Self { resolvers }
    }
}

impl ImageResolver for ImageResolverChain<'_> {
    fn supports(&self, request: &ImageResolveRequest) -> bool {
        self.resolvers
            .iter()
            .any(|resolver| resolver.supports(request))
    }

    fn resolve(&self, request: &ImageResolveRequest) -> Result<ImageResolveReport> {
        let Some(resolver) = self
            .resolvers
            .iter()
            .find(|resolver| resolver.supports(request))
        else {
            return Err(anyhow!(
                "no image resolver supports source {:?} for '{}'",
                request.requirement.image.source,
                request.requirement.image.raw()
            ));
        };
        resolver.resolve(request)
    }
}

#[derive(Debug, Default)]
pub(crate) struct ReferenceOnlyImageResolver;

impl ImageResolver for ReferenceOnlyImageResolver {
    fn supports(&self, request: &ImageResolveRequest) -> bool {
        request.mode == ImageResolutionMode::ReferenceOnly
    }

    fn resolve(&self, request: &ImageResolveRequest) -> Result<ImageResolveReport> {
        let resolved_digest =
            if request.requirement.image.source == ImageReferenceSource::OciRegistry {
                match request.requirement.image.as_oci_registry_reference()?.kind {
                    OciRegistryReferenceKind::Digest(digest) => Some(digest),
                    OciRegistryReferenceKind::TagAndDigest { digest, .. } => Some(digest),
                    OciRegistryReferenceKind::Tag(_) => None,
                }
            } else {
                None
            };
        Ok(ImageResolveReport {
            requirement: request.requirement.clone(),
            resolved_digest,
            platform: request.requirement.platform.clone(),
            manifest_size_bytes: None,
            materialized: false,
        })
    }
}
