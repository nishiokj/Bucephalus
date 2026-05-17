# AgentLab Rust Workspace

This is the implementation workspace for the `lab` binary.

```bash
cargo build --bin lab --release
./target/release/lab --help
```

Common checks:

```bash
cargo check --workspace
cargo test -p lab-schemas
```

The binary is defined by `crates/lab-cli`; runner behavior lives primarily in
`crates/lab-runner`.
