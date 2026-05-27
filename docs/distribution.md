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

## Release artifacts

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
