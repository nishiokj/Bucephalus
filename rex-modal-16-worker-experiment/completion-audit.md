# Completion Audit

This audit records the current setup state for the Rex Modal 8-worker experiment.

## Requirements And Evidence

| Requirement | Current evidence | Status |
| --- | --- | --- |
| Use a new folder | `/Users/jevinnishioka/Desktop/rex-modal-8-worker-experiment` contains the experiment files. | Satisfied |
| Do not modify AgentLab source | `/Users/jevinnishioka/Desktop/Experiments` is clean after reverting the local fallback patch; `cargo test modal_` and `cargo build --release --bin lab` pass from the clean tree. | Satisfied |
| Do not write imperative shims | The agent command in `experiment.yaml` calls `bun /opt/agent/packages/infra/harness-daemon/bin/rex.ts run` directly. No wrapper script is present. | Satisfied |
| Use the Rex agent client CLI | `experiment.yaml` uses Rex's `run` CLI with explicit `--input`, `--output`, `--events`, provider, model, and agent-type args. | Satisfied |
| Two variants | `rex_standard` and `rex_coding` are declared in `experiment.yaml`; `check-package` reports two unique variant ids. | Satisfied |
| About 10 cases | `cases.jsonl` has 10 `case_v2` rows; packaged `tasks/tasks.jsonl` has 10 rows. | Satisfied |
| 3 trials per variant per case | `matrix.repeats: 3` and seeds `[1, 2, 3]`; runner resolves `tasks=10 variants=2 replications=3 total_trials=60`. | Satisfied |
| 8 concurrent workers | `scheduling.max_concurrency: 8`, `AGENTLAB_LOCAL_WORKER_MAX_IN_FLIGHT=8`, and `AGENTLAB_MODAL_MAX_ACTIVE_SANDBOXES=8`; clean-source full run `run_20260522_002448_235196_000001` on the older package launched 60 slots and observed 8 overlapping Modal sandboxes. Current package has not been run remotely after the stop request. | Configured; current package remote validation pending |
| Modal backend | Run command uses `--executor modal`; clean-source full run reached Modal/R2 and completed at the runner level on the older package, then exposed Rex event append incompatibility with R2. Current package moves events to `/tmp` and is locally checked. | Current package remote validation pending |
| Modal/S3 values later | Cloudflare R2 bucket `agentlab-rex-modal-20260521` exists; after replacing the R2 S3 key pair, `put-object`, `head-object`, `list-objects-v2`, and `delete-object` succeed against the bucket endpoint. | Satisfied |
| Keep economics complaints ledger | `complaints-ledger.md` records 20 package economics complaints. | Satisfied |
| Use a new AgentLab binary | `/Users/jevinnishioka/Desktop/Experiments/target/release/lab` was built and used for package checks/probes. | Satisfied |
| Build a new agents image | `docker.io/jevnishioka1/agentlab-rex-modal@sha256:056337363994a9a9c8cff4a0655bdd1da7ed9c64ff8944f38265c0724c8424d7` was built from `/Users/jevinnishioka/Desktop/agent` and pushed to Docker Hub with OCI media types and eStargz compression; anonymous registry manifest access returns HTTP 200. | Satisfied |

## Verified Artifacts

- Package: `.lab/builds/rex-modal-8-worker-r2-tmp-events-2`
- Package digest: `sha256:5ef4fbc1df0f33a2da8d6a897b4aebf4affdb864b159748391769657e23ea927`
- `check-package`: passed with zero warnings; image refs are digest-pinned
- Rex output layout: canonical result remains `/agentlab/out/result.json`; Rex events are declared at `/tmp/rex-events.jsonl` to avoid appending directly to the R2 mount
- Rex image CLI: `rex.ts --help` runs successfully in the image
- Clean-source Modal/R2 smoke before key replacement: `run_20260522_000542_160972_000001` passed preflight, started `slots=2 max_concurrency=8`, created Modal sandbox `sb-c1zDdgsfGv7kOd1LGJEu3h`, then failed with `Sandbox is unavailable` during `/agentlab/in` directory creation on the native R2 CloudBucketMount
- Clean-source full run before the `/tmp` events adjustment: `run_20260522_002448_235196_000001` completed with `ok=true` at the runner level; 60 slots committed, 60 sandboxes recorded/removed, 8 overlapping Modal sandboxes observed, and trial failures were caused by Rex appending JSONL events to `/agentlab/out/rex-events.jsonl` on the R2 CloudBucketMount
- Historical patched Modal smoke: `run_20260521_235551_192954_000001` completed with `ok=true`; both smoke trials reached Rex and failed only because the run intentionally used `OPENAI_API_KEY=dummy`
- Historical patched Modal full schedule validation: `run_20260521_235858_385887_000001` completed with `ok=true`; 60 slots committed, 60 sandboxes recorded/removed, 8 overlapping Modal sandboxes observed, and all 60 agent failures were expected OpenAI 401s from `OPENAI_API_KEY=dummy`
- R2 bucket: `agentlab-rex-modal-20260521` created via Wrangler and smoke-tested with object put/get/delete
- R2 endpoint: `https://2c5c6d868af60c47fe133c773dbb00ee.r2.cloudflarestorage.com`
- R2 S3 API check: after replacing the key pair, `aws s3api put-object`, `head-object`, `list-objects-v2`, and `delete-object` succeed against the R2 bucket endpoint with `--region auto`
- Local Modal launcher Python: `.venv-modal/bin/python` with `modal==1.4.3`
- Current package status: `.lab/builds/rex-modal-8-worker-r2-tmp-events-2` is locally built and checked after moving Rex events to `/tmp/rex-events.jsonl`; no Modal run has been started with this package after the stop request
- Project-local AgentLab DB: `AGENTLAB_HOME=/Users/jevinnishioka/Desktop/rex-modal-8-worker-experiment/.agentlab`; `sqlite3 .agentlab/agentlab.sqlite 'pragma integrity_check;'` returns `ok`

## Remaining External Prerequisites

1. Fill `OPENAI_API_KEY` in `modal.env` or export it before launch if successful Rex answers are required.
2. Run the current package with `--executor modal` when remote sandbox starts are approved.
