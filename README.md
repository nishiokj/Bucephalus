# AgentLab

AgentLab is a Rust CLI for building, running, and inspecting agent evaluations.

It takes an experiment file, seals it into a run package, executes trials through
the runner, stores durable results, and exposes the run through built-in views
and SQL queries.

This repository is the Rust project. Python harnesses, ad hoc benchmark
generators, demos, and local experiment scratch files are not part of the
shipped product.

## Status

AgentLab is not ready for registry distribution yet, but it is close enough to
define the release shape.

The product should ship as one native binary named `lab`. Other registries
should install or wrap that same binary; they should not carry another runtime
implementation.

Current blockers in this repo:

- The binary crate package is named `lab-cli`, while the binary is named `lab`.
  That is fine locally, but public install names need to be chosen deliberately.
- Internal crates use path dependencies. That works in the workspace, but
  crates.io publishing requires every published internal crate to exist in the
  registry with compatible versions, or the binary crate must be restructured.
- `lab-schemas` embeds `../../../schemas`, which is outside its crate directory.
  A publishable crate needs those schemas inside the crate package or generated
  into `OUT_DIR` from package-owned files.
- The workspace has `[patch.crates-io] libduckdb-sys = { path = ... }`.
  Registry publishing cannot depend on a local patch. Either use upstream
  crates.io packages, publish a forked crate, or make DuckDB optional behind a
  feature that release builds can control.
- Release artifacts do not exist yet: no target matrix, checksums, installer
  scripts, Homebrew formula, npm package, or PyPI wrapper.

## Build From Source

```bash
cargo build --manifest-path rust/Cargo.toml --bin lab --release
./rust/target/release/lab --help
```

Useful development checks:

```bash
cargo check --manifest-path rust/Cargo.toml --workspace
cargo test --manifest-path rust/Cargo.toml -p lab-schemas
```

## CLI Shape

```text
lab build <experiment.yaml> --out <package-dir>
lab check-package <package-dir>
lab preflight <package-dir>
lab run <package-dir>
lab build-run <experiment.yaml> --out <package-dir>
lab runs
lab views <run-id-or-dir>
lab query <run-id-or-dir> "SELECT * FROM trials LIMIT 20"
lab schema-validate --schema <schema-name> --file <json-file>
```

Run `lab <command> --help` for command-specific flags.

The main workflow is:

```text
author experiment -> build package -> check/preflight -> run -> inspect
```

## Distribution Plan

Recommended order:

1. GitHub Releases or equivalent binary release channel.
2. Cargo install for Rust-native users.
3. npm global package for JavaScript toolchains.
4. Homebrew tap for macOS/Linux operators.
5. PyPI package for `pipx`, only as a binary installer wrapper.

### Binary Releases

This should be the source of truth for every other installer.

Ship:

```text
lab-aarch64-apple-darwin.tar.gz
lab-x86_64-apple-darwin.tar.gz
lab-x86_64-unknown-linux-gnu.tar.gz
lab-aarch64-unknown-linux-gnu.tar.gz
lab-x86_64-pc-windows-msvc.zip
SHA256SUMS
```

Each archive should contain:

```text
lab
README.md
LICENSE
```

### Cargo

Target command:

```bash
cargo install agentlab-cli
```

Recommended crate naming:

```text
agentlab-cli      binary crate, installs `lab`
agentlab-runner   runner library, only public if external embedding is supported
agentlab-schemas  schema library, only public if schema validation is a public API
```

Do not publish the current workspace as-is. First fix path dependencies,
package-owned schemas, metadata, and the DuckDB patch.

### npm

Target command:

```bash
npm install -g @agentlab/cli
```

The npm package should be a thin installer for the native binary. It should not
bundle the Rust source tree and should not reimplement the CLI in JavaScript.

Reasonable package layout:

```text
npm/
  package.json
  bin/lab.js
  install.js
```

`install.js` downloads the matching release artifact, verifies its checksum,
and places `lab` where `bin/lab.js` can execute it. Optional platform-specific
npm packages can come later if install-time downloads become a problem.

### PyPI / pipx

Target command:

```bash
pipx install agentlab-cli
```

This should be a Python packaging shim only. It may install a console script
named `lab`, but that script should exec the Rust binary. Do not add Python
runner code, benchmark harnesses, or test fixtures back into this repo.

A wheel-per-platform approach is acceptable if it only contains:

```text
lab native binary
small Python entrypoint that execs lab
package metadata
```

### Homebrew

Target command:

```bash
brew install agentlab
```

The formula should install from the binary release artifacts and verify SHA256.
Building from source can be a fallback after Cargo packaging is clean.

## Repository Layout

```text
rust/        Rust workspace for the CLI, runner, schemas, persistence, and views
schemas/     JSON Schema contracts embedded by the Rust schema crate
docs/        Design notes and implementation history retained for maintainers
```

Everything else should earn its place. Generated runs, local demos, scratch
experiments, and benchmark acquisition code should stay ignored or live outside
the product repo.

## Packaging Work Remaining

Before publishing:

1. Choose public names: crate, npm scope, PyPI package, Homebrew tap/formula.
2. Rename or publish around `lab-cli` so install commands are not confusing.
3. Add complete Cargo package metadata: description, repository, homepage,
   readme, keywords, categories, license files.
4. Decide whether internal crates are public crates or private workspace-only
   implementation details.
5. Move embedded schemas into package-owned crate paths.
6. Remove registry-hostile local patches, especially the vendored DuckDB patch,
   or make the vendoring strategy explicit and reproducible.
7. Add release builds for target triples and checksum generation.
8. Add npm and Python installer wrappers only after the binary release artifact
   exists.
9. Keep `demos/`, `.lab/`, generated data, and benchmark scratch out of Git.

## License

MIT
