# Rex Modal 8 Worker Experiment

This folder defines a small AgentLab run for the Rex CLI agent without modifying AgentLab source and without shell wrapper shims.

- 10 cases
- 2 variants: `rex_standard` and `rex_coding`
- 3 repeats per variant per case
- 60 total trials
- 8-way scheduling concurrency
- Modal selected at run time with `--executor modal`

The experiment uses `matrix.tasks` even though the newer docs prefer `matrix.cases`, because the current package runner can retain both aliases after build and then reject the sealed package at run time.

## Build Artifacts

Build the fresh AgentLab binary:

```bash
cd /Users/jevinnishioka/Desktop/Experiments
cargo build --release --bin lab
```

Build and push the fresh Rex image from the agents repo with eStargz compression for Modal fast pulls:

```bash
docker buildx build \
  --builder rex-estargz \
  --platform linux/amd64 \
  --build-context agent_node_modules=/Users/jevinnishioka/Desktop/agent/node_modules \
  --build-context executioner_js=/Users/jevinnishioka/Desktop/substrate/packages/executioner-js \
  -f /Users/jevinnishioka/Desktop/rex-modal-8-worker-experiment/Dockerfile.rex-agent \
  --tag docker.io/jevnishioka1/agentlab-rex-modal:2026-05-21 \
  --output type=registry,compression=estargz,force-compression=true,oci-mediatypes=true \
  /Users/jevinnishioka/Desktop/agent
```

The current experiment is pinned to the pushed Docker Hub OCI/eStargz image reference.

Registry image:

```text
docker.io/jevnishioka1/agentlab-rex-modal@sha256:056337363994a9a9c8cff4a0655bdd1da7ed9c64ff8944f38265c0724c8424d7
index media type: application/vnd.oci.image.index.v1+json
platform: linux/amd64
layers include containerd.io/snapshot/stargz/toc.digest annotations
anonymous Docker Registry manifest request returns HTTP 200
```

## Package And Run

Build and check the package:

```bash
cd /Users/jevinnishioka/Desktop/rex-modal-8-worker-experiment
set -a
. ./modal.env
set +a
/Users/jevinnishioka/Desktop/Experiments/target/release/lab build experiment.yaml --out .lab/builds/rex-modal-8-worker-runnable-N --json
/Users/jevinnishioka/Desktop/Experiments/target/release/lab check-package .lab/builds/rex-modal-8-worker-runnable-N --json
```

The build output directory must not already exist or must be empty, so replace `N` with a fresh suffix for each rebuild.

Last verified package:

```text
.lab/builds/rex-modal-8-worker-r2-tmp-copy-events-resources-1
sha256:eb142e49d9e60e2294cddd973550367aa66d27e3ba28eb7047d4736a0e159a2e
```

`check-package` passed with zero warnings; the task image ref is digest-pinned. The Rex result file remains at AgentLab's canonical `/agentlab/out/result.json`; only the append-heavy Rex events stream is written to `/tmp/rex-events.jsonl` during execution, then copied to `/agentlab/out/rex-events.jsonl` after Rex exits to avoid append semantics on the R2 object-store mount.

The YAML declares per-sandbox resources under `policy.task_sandbox.resources`:

```yaml
cpu_count: 2
memory_mb: 4096
```

The Modal launcher passes those through to `modal.Sandbox.create(cpu=2.0, memory=4096)`.

`modal.env` sets `AGENTLAB_HOME=/Users/jevinnishioka/Desktop/rex-modal-8-worker-experiment/.agentlab`. Keep that loaded for build/check/run commands so AgentLab uses the project-local account database. The project-local database passed `sqlite3 .agentlab/agentlab.sqlite 'pragma integrity_check;'` with `ok`. Without this env, AgentLab falls back to its default `$HOME/.agentlab/agentlab.sqlite`; on this machine that path has produced an early `configure sqlite pragmas` failure.

Latest package check:

```text
12 checks, 0 failed, 3 skipped, 0 warnings
```

Load the env file and launch:

```bash
cd /Users/jevinnishioka/Desktop/rex-modal-8-worker-experiment
set -a
. ./modal.env
set +a
/Users/jevinnishioka/Desktop/Experiments/target/release/lab run .lab/builds/rex-modal-8-worker-r2-tmp-copy-events-resources-1 --executor modal --materialize full --json
```

For infrastructure-only smoke without a real OpenAI key:

```bash
export OPENAI_API_KEY=dummy
/Users/jevinnishioka/Desktop/Experiments/target/release/lab run .lab/builds/rex-modal-8-worker-r2-tmp-copy-events-resources-1 --executor modal --smoke-test --materialize full --json
```

Historical Modal smoke with a temporary local launcher patch:

```text
run_id=run_20260521_235551_192954_000001
ok=true
preflight=20 passed, 0 warnings, 0 failed
schedule=smoke-test slots=2 max_concurrency=8
result=Modal sandbox creation, staging, exec, collection, and cleanup succeeded
agent failures=expected OpenAI 401 from OPENAI_API_KEY=dummy
```

Historical full schedule validation with dummy provider key and a temporary local launcher patch:

```text
run_id=run_20260521_235858_385887_000001
ok=true
preflight=20 passed, 0 warnings, 0 failed
schedule=60 slots, max_concurrency=8
trials=60 committed
modal_sandboxes=60 created and removed
observed_modal_overlap=8 sandboxes
agent failures=60 expected OpenAI 401s from OPENAI_API_KEY=dummy
```

Clean-source Modal/R2 validation before the `/tmp` events adjustment:

```text
run_id=run_20260522_000542_160972_000001
binary=/private/tmp/agentlab-clean-modal/target/release/lab built from clean AgentLab HEAD
preflight=20 passed, 0 warnings, 0 failed
schedule=smoke-test slots=2 max_concurrency=8
result=failed during Modal sandbox staging
error=Sandbox is unavailable after native R2 CloudBucketMount startup
```

After replacing the R2 S3 key pair, direct R2 S3 API validation now passes:

```text
put-object -> succeeded
head-object -> succeeded
list-objects-v2 with probe prefix -> succeeded
delete-object -> succeeded
```

Clean-source full validation using the older package and fixed R2 keys:

```text
run_id=run_20260522_002448_235196_000001
ok=true at runner level
schedule=60 slots, max_concurrency=8
trials=60 committed
modal_sandboxes=60 created and removed
observed_modal_overlap=8 sandboxes
trial failure mode=Rex append to /agentlab/out/rex-events.jsonl failed with EPERM on R2 CloudBucketMount
```

The current package fixes that trial failure mode locally by moving Rex events to `/tmp/rex-events.jsonl`. No Modal run has been started with this package after the stop request.

The YAML keeps `runtime.compute.backend: local-docker` because the current package validation docs list only `local-docker`, `local-fs`, and `local-stdout` as declarative runtime backends. The Modal path is exposed by the CLI executor flag.

## Cloudflare R2

Wrangler is authenticated and the R2 bucket exists:

```text
account_id=2c5c6d868af60c47fe133c773dbb00ee
bucket=agentlab-rex-modal-20260521
endpoint=https://2c5c6d868af60c47fe133c773dbb00ee.r2.cloudflarestorage.com
region=auto
force_path_style=true
```

The bucket was smoke-tested with `wrangler r2 object put/get/delete`.

`modal.env` and `modal.env.example` are configured for this R2 bucket and for the local Modal launcher venv:

```text
AGENTLAB_MODAL_PYTHON=/Users/jevinnishioka/Desktop/rex-modal-8-worker-experiment/.venv-modal/bin/python
```

The R2 fields are the S3 credentials from a Cloudflare R2 API token with Object Read & Write access:

```text
AWS_ACCESS_KEY_ID=
AWS_SECRET_ACCESS_KEY=
```

Cloudflare shows these as the R2 token's Access Key ID and Secret Access Key when the token is created.

Modal `CloudBucketMount` against R2 does not behave like a normal POSIX filesystem. It can stage whole files through the mount, but Rex's incremental JSONL append to a mounted object path failed with `EPERM`; the current package keeps the canonical result on `/agentlab/out/result.json` and writes Rex events to `/tmp/rex-events.jsonl` for AgentLab to collect after the process exits.
