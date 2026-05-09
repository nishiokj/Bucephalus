from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]


def load_module(relative: str, name: str):
    path = ROOT / relative
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def write_json(path: Path, payload) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload) + "\n", encoding="utf-8")


def read_jsonl(path: Path) -> list[dict]:
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def swebench_row(instance_id: str, repo: str = "django/django") -> dict:
    return {
        "repo": repo,
        "instance_id": instance_id,
        "base_commit": "abc123",
        "problem_statement": "Fix the failing behavior.",
        "hints_text": "",
        "created_at": "2024-01-01T00:00:00Z",
        "version": "1.0",
        "environment_setup_commit": "def456",
        "test_patch": "diff --git a/test.py b/test.py\n",
        "FAIL_TO_PASS": ["tests/test_regression.py::test_bug"],
        "PASS_TO_PASS": ["tests/test_existing.py::test_ok"],
    }


def test_acquire_swebench_lite_writes_tasks_and_metadata(tmp_path: Path) -> None:
    source = tmp_path / "source.jsonl"
    rows = [
        swebench_row("django__django-11019"),
        swebench_row("astropy__astropy-12907", repo="astropy/astropy"),
    ]
    source.write_text("".join(json.dumps(row) + "\n" for row in rows), encoding="utf-8")

    output = tmp_path / "tasks.jsonl"
    raw_output = tmp_path / "raw.jsonl"
    metadata_dir = tmp_path / "metadata"
    ids = tmp_path / "ids.txt"
    meta = tmp_path / "meta.json"

    subprocess.run(
        [
            sys.executable,
            str(ROOT / "scripts/acquire_swebench_lite.py"),
            "--source-jsonl",
            str(source),
            "--count",
            "2",
            "--output",
            str(output),
            "--raw-output",
            str(raw_output),
            "--metadata-dir",
            str(metadata_dir),
            "--ids",
            str(ids),
            "--meta-output",
            str(meta),
        ],
        check=True,
        cwd=str(ROOT),
    )

    task_rows = read_jsonl(output)
    assert len(task_rows) == 2
    assert task_rows[0]["schema_version"] == "task_row_v1"
    assert task_rows[0]["image"] == "swebench/sweb.eval.x86_64.astropy__astropy-12907:latest"
    assert task_rows[0]["workdir"] == "/testbed"
    assert task_rows[0]["task"]["swebench"]["input"]["instance_id"] == "astropy__astropy-12907"
    assert (metadata_dir / "astropy__astropy-12907.json").exists()
    assert (metadata_dir / "django__django-11019.json").exists()
    assert read_jsonl(raw_output)[0]["test_patch"].startswith("diff --git")
    assert json.loads(meta.read_text(encoding="utf-8"))["selected_rows"] == 2


def test_acquire_swebench_lite_rejects_rows_without_grader_metadata(tmp_path: Path) -> None:
    source = tmp_path / "source.jsonl"
    row = swebench_row("django__django-11019")
    del row["test_patch"]
    source.write_text(json.dumps(row) + "\n", encoding="utf-8")

    proc = subprocess.run(
        [
            sys.executable,
            str(ROOT / "scripts/acquire_swebench_lite.py"),
            "--source-jsonl",
            str(source),
            "--count",
            "1",
            "--output",
            str(tmp_path / "tasks.jsonl"),
            "--ids",
            str(tmp_path / "ids.txt"),
        ],
        cwd=str(ROOT),
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    assert proc.returncode != 0
    assert "missing official grader field" in proc.stderr


def test_official_eval_skip_harness_writes_agentlab_predictions(tmp_path: Path) -> None:
    run_dir = tmp_path / "run_1"
    trial_dir = run_dir / "trials/trial_1"
    write_json(
        trial_dir / "in/trial_input.json",
        {
            "ids": {
                "run_id": "run_1",
                "trial_id": "trial_1",
                "variant_id": "gpt_5_4_low",
                "task_id": "swebench_django_django_11019",
                "repl_idx": 0,
            },
            "task": {
                "id": "swebench_django_django_11019",
                "benchmark": {
                    "adapter_id": "swebench_official_harness",
                    "name": "swebench_lite",
                    "split": "test",
                },
                "swebench": {
                    "input": {
                        "instance_id": "django__django-11019",
                    }
                },
            },
        },
    )
    write_json(
        trial_dir / "out/result.json",
        {
            "schema_version": "artifact_envelope_v1",
            "artifact_type": "patch_submission",
            "artifact": {
                "patch": "diff --git a/a.py b/a.py\n",
            },
        },
    )
    write_json(trial_dir / "trial_metadata.json", {"ids": {"task_index": 7}})

    output_dir = tmp_path / "official"
    subprocess.run(
        [
            sys.executable,
            str(ROOT / "scripts/run_official_swebench_eval_from_agentlab.py"),
            str(run_dir),
            "--output-dir",
            str(output_dir),
            "--skip-harness",
        ],
        check=True,
        cwd=str(ROOT),
    )

    swe_rows = read_jsonl(output_dir / "gpt_5_4_low/predictions.jsonl")
    assert swe_rows == [
        {
            "instance_id": "django__django-11019",
            "model_name_or_path": "gpt_5_4_low",
            "model_patch": "diff --git a/a.py b/a.py\n",
        }
    ]
    agentlab_rows = read_jsonl(output_dir / "gpt_5_4_low/agentlab_predictions.jsonl")
    assert agentlab_rows[0]["schema_version"] == "benchmark_prediction_record_v1"
    assert agentlab_rows[0]["schedule_idx"] == 7
    assert agentlab_rows[0]["prediction"]["kind"] == "patch"


def test_official_eval_report_mapping_to_agentlab_score() -> None:
    module = load_module(
        "scripts/run_official_swebench_eval_from_agentlab.py",
        "run_official_swebench_eval_from_agentlab_test",
    )
    verdict, value, ext = module.verdict_from_report(
        {
            "resolved_instances": ["django__django-11019"],
            "unresolved_instances": ["astropy__astropy-12907"],
        },
        "django__django-11019",
    )

    assert verdict == "pass"
    assert value == 1.0
    assert ext["official_status"] == "resolved"


def test_official_eval_scopes_candidate_patch_to_source_files() -> None:
    module = load_module(
        "scripts/run_official_swebench_eval_from_agentlab.py",
        "run_official_swebench_eval_from_agentlab_scope_test",
    )
    patch = """diff --git a/astropy/modeling/separable.py b/astropy/modeling/separable.py
--- a/astropy/modeling/separable.py
+++ b/astropy/modeling/separable.py
@@ -1 +1 @@
-old
+new
diff --git a/astropy/modeling/tests/test_separable.py b/astropy/modeling/tests/test_separable.py
--- a/astropy/modeling/tests/test_separable.py
+++ b/astropy/modeling/tests/test_separable.py
@@ -1 +1 @@
-old test
+new test
diff --git a/pyproject.toml b/pyproject.toml
--- a/pyproject.toml
+++ b/pyproject.toml
@@ -1 +1 @@
-requires = ["setuptools"]
+requires = ["setuptools==68.0.0"]
"""

    scoped, diagnostics = module.scope_swebench_candidate_patch(patch)

    assert "astropy/modeling/separable.py" in scoped
    assert "astropy/modeling/tests/test_separable.py" not in scoped
    assert "pyproject.toml" not in scoped
    assert diagnostics["policy"] == "swebench_candidate_source_patch_v1"
    assert diagnostics["included_files"] == ["astropy/modeling/separable.py"]
    assert diagnostics["excluded_files"] == [
        {"path": "astropy/modeling/tests/test_separable.py", "reason": "test_file"},
        {"path": "pyproject.toml", "reason": "dependency_or_tooling_metadata"},
    ]
