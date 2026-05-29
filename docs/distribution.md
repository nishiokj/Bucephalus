# Distribution & Packaging

How Bucephalus is built, packaged, and released. The README covers day-to-day
local use; this document is the packaging boundary.

## Install

From a checkout:

```bash
cargo install --path .
```

After install the primary command is `bucephalus`:

```bash
bucephalus --help
```

The legacy `lab` executable is still installed as a compatibility alias.

The intended Cargo registry package is `bucephalus-cli` and the intended Homebrew
formula is `bucephalus`. Neither is published yet — no public tap or release
artifact exists today.

## Build from source

```bash
cargo build --release --bin bucephalus
./target/release/bucephalus --help
```

Useful checks:

```bash
cargo check
cargo package --allow-dirty
```

`cargo package` is the registry boundary check: it verifies the crate can be
unpacked and built from only the files that would ship to crates.io.

## Distribution shape

Bucephalus ships as one publishable Rust crate:

```text
package: bucephalus-cli
binary:  bucephalus
alias:   lab
formula: bucephalus
```

The crate root is the repository root. The Rust implementation stays under
`rust/crates/*`, but those directories are a private source layout, not
separately published crates.

The shipped product is the core CLI. The DuckDB-backed analysis commands
(`bucephalus views`, `bucephalus query`) are a local-only build behind the `duckdb_engine`
feature — they are not part of the default package. DuckDB is bundled and
compiled from source, which is why it is kept out of the default build:

```bash
cargo build --release --features duckdb_engine --bin bucephalus
```

A cargo alias is wired up for this: `cargo bucephalus-full`.

Homebrew is planned, not available today. It should ship prebuilt release
archives of the core CLI.

## Core release artifacts

Publish archives like:

```text
bucephalus-aarch64-apple-darwin.tar.gz
bucephalus-x86_64-apple-darwin.tar.gz
bucephalus-x86_64-unknown-linux-gnu.tar.gz
bucephalus-aarch64-unknown-linux-gnu.tar.gz
SHA256SUMS
```

Each archive should contain `bucephalus`, `README.md`, and `LICENSE`. The Homebrew
formula should download the matching archive, verify SHA256, and install `bucephalus`.

## Cloud runner release artifacts

Bucephalus Cloud uses a larger release bundle because runner VMs need both the
Core binary and the Cloud worker/controller code. Build it with:

```bash
scripts/release/build-buc-release.sh --version 0.3.1
```

The Linux release workflow builds the provider-facing shape for
`x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`:

```text
dist/releases/bucephalus-<version>-<target>/
  bin/bucephalus
  bucephalus-cloud/
    src/
    api/openapi/
    db/migrations/
    deploy/runner-vm/
    deploy/pool-controller/
    deploy/runner-image/
    package.json
    bun.lock
    tsconfig.json
  release-manifest.json
  SHA256SUMS
```

The matching archive is:

```text
bucephalus-<version>-<target>.tar.gz
bucephalus-<version>-<target>.tar.gz.sha256
```

Runner image baking should consume this bundle, not a live checkout. The baked
image installs `bin/bucephalus` as `/usr/local/bin/bucephalus`, installs the
`bucephalus-cloud` directory at `/opt/bucephalus-cloud`, runs
`bun install --frozen-lockfile`, installs the systemd unit, and records the
release manifest in the image metadata.

The Cloud release bundle is a VM image input, not the final runtime boundary:
provider secrets, pool id, provision request id, and provider instance id are
still injected when the VM is created.

Runner image bakes should also declare their scheduling shape through
`BUCEPHALUS_WORKER_ARCH`, `BUCEPHALUS_WORKER_CPU_COUNT`,
`BUCEPHALUS_WORKER_MEMORY_MB`, `BUCEPHALUS_WORKER_DISK_MB`, and
`BUCEPHALUS_WORKER_ISOLATION`. Cloud run requirements use the same fields when
matching queued runs to runner pools and runner instances.

## CI/CD

The Cloud/Core CI gate is:

```bash
scripts/ci/cloud-gates.sh
```

It checks Rust formatting and tests, Cloud typecheck and tests, OpenAPI YAML
parseability plus local `$ref` targets, and Postgres migrations when
`DATABASE_URL` is set.

Official release builds require a clean git worktree. Local smoke builds may set
`BUCEPHALUS_RELEASE_ALLOW_DIRTY=true`.

GitHub Actions wires this into:

- `.github/workflows/bucephalus-cloud-ci.yml`: PR and main CI for Core + Cloud.
- `.github/workflows/bucephalus-release.yml`: manual/tagged Linux release bundle
  build, uploaded as a GitHub Actions artifact.

## Repository boundary

This repository is the Rust product. Python harnesses, demos, generated runs,
and local experiment scratch do not ship.

The package includes:

```text
Cargo.toml
README.md
LICENSE
schemas/
rust/crates/*/src/
rust/crates/lab-analysis/views/
```

Ignored or removed from the product surface:

```text
.lab/
demos/
Python scripts and package metadata
legacy docs and generated benchmark artifacts
```
