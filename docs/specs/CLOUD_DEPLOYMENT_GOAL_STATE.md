# Cloud Deployment Goal State

This document defines the target deployment shape for Bucephalus Cloud. It is
intended to guide parallel implementation work without prescribing every tool
choice or local trick. The goal is to remove ambiguity, expose real blockers
early, and prevent prototype deployment shims from becoming architecture.

The target is a real cloud deployment, initially one cloud, with clean
separation between infrastructure, build artifacts, deploy promotion, and
runtime experiment execution. Local development remains useful for unit tests,
schema iteration, and CLI ergonomics, but local VM deployment is not the proof
of production readiness.

## Core Thesis

Terraform or equivalent infrastructure-as-code owns durable cloud resources.
CI/CD produces immutable artifacts and images. Deploy promotes selected
artifacts into already-declared infrastructure. Runtime orchestration allocates
bounded experiment capacity through the Cloud API.

The system may be blocked when credentials, cloud accounts, DNS, managed
database access, or secret-manager policy are missing. A clean block is better
than a workaround that hides the boundary.

For local operator-driven work, loading credentials from an uncommitted `.env`
into a tool process is acceptable. That is an input convenience, not a
deployment boundary, and it must not become persisted VM metadata, checked-in
examples, image content, or long-lived runtime configuration.

## Non-Goals

- Do not support Mac Studio, OrbStack, or local Linux VM deployment as a
  production-shaped path.
- Do not preserve SSH/scp deploy workflows as a fallback.
- Do not inject production secrets through startup scripts, VM metadata, checked
  env examples, or long-lived local env files.
- Do not make runner VMs direct Postgres clients in the goal state.
- Do not implement both GCP and AWS before one cloud path is clean.
- Do not let experiments freely create arbitrary cloud resources from inside a
  runner VM.

## Pathway 1: Cloud Substrate And Deploy Boundary

Own the durable cloud environment as discrete declared resources.

Large units of work:

- Declare the cloud substrate with infrastructure-as-code: network, subnets,
  private database, artifact registry, secret manager, service identities,
  firewall policy, logging, metrics, and any required object storage.
- Define the control-plane runtime target for API and pool controller services.
  The target may be a VM, managed container service, or another cloud-native
  service, but its identity and network policy must be declared outside the app
  artifact.
- Define database ownership: admin migration identity, runtime API identity,
  and any read-only or operational identities.
- Define deployment promotion: migrate, update service artifact references,
  smoke, observe, and roll back by returning to a previous artifact reference.
- Replace ad hoc SSH deploy with a deploy contract that is explicit about the
  environment, artifact digest, migration identity, and smoke result.

Quality expectations:

- A new agent can answer "what cloud resources exist and why?" by reading the
  infra declaration and this spec.
- Destroying/recreating a non-production environment should not require hidden
  machine state.
- Secrets are referred to by names/paths and fetched by runtime identity; raw
  values are not embedded in infra state when avoidable.
- It is acceptable for a deploy to stop with "missing credential" or "missing
  cloud project" rather than inventing local stand-ins.

Invariants:

- Infrastructure owns where things run.
- Application artifacts do not own network topology, cloud credentials, DB host
  decisions, or secret values.
- Deploy changes versioned references and controlled schema state; it does not
  mutate hosts by hand.
- Postgres is private. Public database exposure is a failed boundary.

## Pathway 2: Artifact, Image, And CI Boundary

Make build output immutable, promotable, and independent of deployment
environment.

Large units of work:

- Build Core, Cloud API, pool controller, worker code, and migrations into
  immutable release artifacts.
- Build API/controller images and runner images from generated per-component
  contexts derived from those artifacts, with prebuilt runtime bundles, no
  per-image dependency install, and no second build between boundary inspection
  and push.
- Publish images/artifacts to a cloud registry with digest-addressable identity.
- Keep image contents focused on code and runtime dependencies: binaries,
  worker code, OS packages, Docker/runtime dependencies when needed, certificate
  bundles, and installed helper binaries such as Tailscale if the network
  provider requires it.
- Keep environment-specific values out of images: DB URLs, auth tokens, cloud
  project IDs unless harmless labels, Tailscale auth keys, user secrets,
  registry credentials, and API base URLs.
- Establish CI gates for typecheck, tests, migrations, image build, artifact
  metadata, and release provenance.

Quality expectations:

- A deploy chooses a specific artifact or image digest. "Latest" is not a
  production deploy input.
- Rebuilding unchanged app code should reuse dependency layers where the build
  system supports it.
- Runtime configuration is injected or retrieved by identity at launch, not
  baked into the image.
- The same artifact can be promoted across dev, staging, and production with
  different cloud policy.

Invariants:

- CI builds things; it does not configure the cloud environment except through
  publishing artifacts to declared destinations.
- Images are not secret stores.
- Images are not network policy.
- Artifact metadata must be sufficient to explain what source revision and
  dependency lockfiles produced a runtime.

## Pathway 3: Runtime Control Plane, Runner Capacity, And User Secrets

Make experiment execution a first-class cloud runtime rather than a VM-local
script bundle.

Large units of work:

- Move runner interaction to the Cloud API. Runner VMs should register, claim
  work, heartbeat, receive package materialization instructions, emit events,
  upload outputs, and report cleanup status through API-owned endpoints.
- Remove direct runner Postgres access from the goal state. The API owns DB
  writes, leases, queue state, runtime persistence, and event ingestion.
- Define runner capacity provisioning as a cloud-provider boundary owned by the
  pool controller: select capacity, create runner identity, attach network
  policy, and start the worker process.
- Keep per-run runner boot work minimal: use a container-ready boot image with
  Docker preinstalled by default, and reserve package installation for explicit
  custom image fallbacks.
- Define runtime resource requirements as declared run input: CPU, memory, disk,
  GPU, architecture, isolation mode, container/image requirements, sidecars,
  registry access, network perimeter, and secret refs.
- Build a first-class user-secret highway: declare secret refs in the package,
  authorize only the attempt that needs them, materialize as files or scoped env,
  redact logs/events, and destroy after cleanup.
- Define runtime network policy: what the runner VM may reach, what the attempt
  container may reach, what egress is denied by default, and how provider-native
  services are attached.
- Preserve poisoning/cleanup semantics: failed cleanup makes the runner
  unhealthy and unavailable for more work.

Quality expectations:

- A runner can do its job with API access, registry/object-storage access when
  declared, and secret access scoped to its identity and attempt.
- A compromised runner token should not imply database ownership.
- User secrets are auditable by reference and scope without revealing plaintext.
- Experiment runtime provisioning is explicit enough that missing policy is
  visible as a block, not silently bypassed.

Invariants:

- The API owns durable state.
- Runners do not own database credentials in the goal state.
- User secrets flow through declared refs and scoped runtime authorization.
- Runtime egress is declared and enforced; it is not an ambient property of the
  VM.
- Capacity provisioning can fail cleanly without corrupting run state.

## Retired Shim Surface

The old local/SSH/startup-script deployment materials are intentionally retired
because they encourage the wrong mental model. They may contain useful facts,
but they are not valid implementation templates for the goal state.

Retired patterns:

- SSH/scp control-plane deploy workflows.
- Mac Studio or OrbStack as a production-shaped deployment path.
- Root shell installers that mutate `/opt`, `/etc`, and systemd as the deploy
  contract.
- Runner bootstrap scripts that persist worker tokens and database URLs into
  local env files.
- GCP startup scripts that install dependencies, inject secrets, join private
  networking, download releases, and start workers in one path.
- Env examples that present long-lived shared secrets as normal configuration.

If an agent needs a retired fact, copy the fact into a new design or
implementation artifact with the new boundary made explicit. Do not revive the
retired script as a shortcut.

## Parallel Work Split

Use these as three independent work paths.

Path 1 owner: cloud substrate and deploy boundary.

- Produce the first cloud infra skeleton.
- Define service identities and secret names.
- Define deploy promotion and migration responsibilities.
- Stop when real cloud credentials, DNS, or account decisions are required.

Path 2 owner: artifact and image boundary.

- Reshape release output and CI around immutable artifacts and images.
- Define artifact metadata and digest promotion.
- Remove build-time dependency on local deployment scripts.
- Stop when registry credentials or cloud project wiring are required.

Path 3 owner: runtime runner and secret boundary.

- Move runner/DB coupling toward API-owned state.
- Design and implement the attempt-scoped user-secret flow.
- Define runtime network/resource declarations and enforcement points.
- Stop when provider policy or real secret-manager integration is required.

## Acceptance Bar

The goal state is credible when:

- A clean cloud environment can be declared without local VM assumptions.
- CI can publish immutable artifacts/images without knowing production secrets.
- Deploy can promote a chosen digest and run migrations with a scoped identity.
- Runner VMs do not need direct Postgres credentials.
- Runtime user secrets have a declared, scoped, redacted, and cleaned-up path.
- Missing credentials or cloud account setup blocks clearly instead of being
  patched around with local shims.
