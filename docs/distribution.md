# Distribution & Packaging

How AgentLab is built, packaged, and released. The README covers day-to-day
local use; this document is the packaging boundary.

## Install

From a checkout:

```bash
cargo install --path .
```

After install the command is `lab`:

```bash
lab --help
```

The intended Cargo registry package is `agentlab-cli` and the intended Homebrew
formula is `agentlab`. Neither is published yet — no public tap or release
artifact exists today.

## Build from source

```bash
cargo build --release --bin lab
./target/release/lab --help
```

Useful checks:

```bash
cargo check
cargo package --allow-dirty
```

`cargo package` is the registry boundary check: it verifies the crate can be
unpacked and built from only the files that would ship to crates.io.

## Distribution shape

AgentLab ships as one publishable Rust crate:

```text
package: agentlab-cli
binary:  lab
formula: agentlab
```

The crate root is the repository root. The Rust implementation stays under
`rust/crates/*`, but those directories are a private source layout, not
separately published crates.

The shipped product is the core CLI. The DuckDB-backed analysis commands
(`lab views`, `lab query`) are a local-only build behind the `duckdb_engine`
feature — they are not part of the default package. DuckDB is bundled and
compiled from source, which is why it is kept out of the default build:

```bash
cargo build --release --features duckdb_engine --bin lab
```

A cargo alias is wired up for this: `cargo lab-full`.

Homebrew is planned, not available today. It should ship prebuilt release
archives of the core CLI.

## Release artifacts

Publish archives like:

```text
lab-aarch64-apple-darwin.tar.gz
lab-x86_64-apple-darwin.tar.gz
lab-x86_64-unknown-linux-gnu.tar.gz
lab-aarch64-unknown-linux-gnu.tar.gz
SHA256SUMS
```

Each archive should contain `lab`, `README.md`, and `LICENSE`. The Homebrew
formula should download the matching archive, verify SHA256, and install `lab`.

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
