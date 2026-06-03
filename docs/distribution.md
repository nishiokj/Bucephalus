# Distribution & Packaging

How Bucephalus is built, packaged, and released. The README covers user-facing
installation and first-run flow; this document is the packaging boundary.

## Public install

The public installer is the only command users should need:

```bash
curl -fsSL https://raw.githubusercontent.com/nishiokj/Bucephalus/main/scripts/install.sh | sh
```

It detects macOS/Linux plus arm64/x86_64, downloads the matching GitHub Release
archive, verifies the archive checksum, and installs `bucephalus` into
`$HOME/.local/bin` unless `BUCEPHALUS_INSTALL_DIR` is set.

Install a specific release with:

```bash
curl -fsSL https://raw.githubusercontent.com/nishiokj/Bucephalus/main/scripts/install.sh | env BUCEPHALUS_VERSION=0.3.1 sh
```

From a checkout, the source install remains:

```bash
cargo install --path .
```

The legacy `lab` executable is still installed as a compatibility alias when
installing from Cargo. Public release archives ship only the `bucephalus`
binary.

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

The analysis commands (`bucephalus views`, `bucephalus query`) read the account
SQLite database directly through the bundled `rusqlite` engine, so they are part
of the default build — there is no separate analytics feature or binary.

## Core release artifacts

GitHub tag releases attach prebuilt core CLI archives for macOS and Linux:

```text
bucephalus-aarch64-apple-darwin.tar.gz
bucephalus-x86_64-apple-darwin.tar.gz
bucephalus-x86_64-unknown-linux-gnu.tar.gz
bucephalus-aarch64-unknown-linux-gnu.tar.gz
<archive>.sha256
```

Each archive contains `bucephalus`, `README.md`, `LICENSE`,
`release-manifest.json`, and `SHA256SUMS`. Build one locally with:

```bash
scripts/release/build-core-release.sh --version 0.3.1 --target aarch64-apple-darwin
```

The Homebrew formula can consume the same archive for its target, verify SHA256,
and install `bucephalus`.

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
    docker-compose.yml
    deploy/control-plane/
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

Control-plane VMs and runner image baking should consume this bundle, not a
live checkout. The control-plane installer places the release under
`/opt/bucephalus/releases/<release>`, updates `/opt/bucephalus/current`, installs
API and pool-controller systemd units, and can run migrations from the
admin/deploy side. The baked runner image installs `bin/bucephalus` as
`/usr/local/bin/bucephalus`, installs the `bucephalus-cloud` directory at
`/opt/bucephalus-cloud`, runs `bun install --frozen-lockfile`, installs the
runner systemd unit, and records the release manifest in the image metadata.

The Cloud release bundle is a VM input, not the final runtime boundary:
control-plane env, provider secrets, pool id, provision request id, and provider
instance id are still injected or configured when the VM is created.

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
