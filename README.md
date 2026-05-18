# AgentLab

AgentLab is a Rust CLI for building, running, and inspecting agent evaluation
experiments.

The installed command is `lab`.

```bash
lab build experiment.yaml --out .lab/packages/example
lab check-package .lab/packages/example
lab run .lab/packages/example
lab views .lab/runs/<run-id>
```

## Install

From this checkout:

```bash
cargo install --path .
```

After install, the command is:

```bash
lab --help
```

The intended Cargo registry package is `agentlab-cli`, but it has not been
published yet.

The intended Homebrew formula is `agentlab`, but no public tap or release
artifact exists yet.

## Build From Source

```bash
cargo build --release --bin lab
./target/release/lab --help
```

Useful checks:

```bash
cargo check
cargo package --allow-dirty
```

`cargo package` is the registry boundary check: it verifies that the crate can
be unpacked and built from only the files that would ship to crates.io.

## Distribution Shape

AgentLab ships as one publishable Rust crate:

```text
package: agentlab-cli
binary:  lab
formula: agentlab
```

The crate root is the repository root. The Rust implementation remains under
`rust/crates/*`, but those directories are private source layout, not separate
published crates.

Default Cargo installs build the core CLI without DuckDB-backed analysis views.
The `duckdb_engine` feature is reserved for release builds that can control the
DuckDB toolchain:

```bash
cargo build --release --features duckdb_engine --bin lab
```

Homebrew is planned, not available today. It should install prebuilt release
archives first. Building from source is acceptable only after the DuckDB
dependency strategy is reproducible on the target platform.

## Release Artifacts

Publish archives like:

```text
lab-aarch64-apple-darwin.tar.gz
lab-x86_64-apple-darwin.tar.gz
lab-x86_64-unknown-linux-gnu.tar.gz
lab-aarch64-unknown-linux-gnu.tar.gz
SHA256SUMS
```

Each archive should contain:

```text
lab
README.md
LICENSE
```

The Homebrew formula should download the matching archive, verify SHA256, and
install `lab`.

## Repository Boundary

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

## License

MIT
