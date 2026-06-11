#!/usr/bin/env python3
"""Generate Bucephalus cases from AgentBench LTP XLSX data."""

from __future__ import annotations

import argparse
import json
import re
import zipfile
from pathlib import Path
from xml.etree import ElementTree as ET


NS = {"m": "http://schemas.openxmlformats.org/spreadsheetml/2006/main"}


def cell_column(cell_ref: str) -> str:
    return re.sub(r"\d+", "", cell_ref)


def read_xlsx_rows(path: Path) -> list[dict[str, str]]:
    with zipfile.ZipFile(path) as archive:
        shared = ET.fromstring(archive.read("xl/sharedStrings.xml"))
        strings: list[str] = []
        for item in shared.findall("m:si", NS):
            strings.append("".join(text.text or "" for text in item.findall(".//m:t", NS)))

        sheet = ET.fromstring(archive.read("xl/worksheets/sheet1.xml"))
        rows: list[dict[str, str]] = []
        headers: dict[str, str] = {}
        for row_index, row in enumerate(sheet.findall(".//m:row", NS), start=1):
            values: dict[str, str] = {}
            for cell in row.findall("m:c", NS):
                ref = cell.attrib.get("r", "")
                col = cell_column(ref)
                value_node = cell.find("m:v", NS)
                value = "" if value_node is None else value_node.text or ""
                if cell.attrib.get("t") == "s" and value:
                    value = strings[int(value)]
                if row_index == 1:
                    headers[col] = value
                else:
                    values[headers.get(col, col)] = value
            if row_index > 1 and values:
                rows.append(values)
        return rows


def split_keys(value: str) -> list[str]:
    keys: list[str] = []
    for line in re.split(r"\r?\n+", value):
        cleaned = re.sub(r"^\s*\d+[\).\s-]*", "", line).strip()
        if cleaned:
            keys.append(cleaned)
    return keys


def build_prompt(story: str) -> str:
    return f"""You are solving an AgentBench Lateral Thinking Puzzle.

Puzzle:
{story}

Infer the hidden situation behind the puzzle. Return exactly one JSON object,
with no Markdown and no code fences, using this shape:
{{"answer": "<your concise explanation>", "key_facts": ["<fact>", "..."]}}

The answer should recover the important hidden facts, not merely restate the
puzzle."""


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--agentbench-root",
        type=Path,
        default=Path("../../../AgentBench"),
        help="Path to the upstream AgentBench checkout.",
    )
    parser.add_argument("--split", default="dev", choices=["dev", "standard"])
    parser.add_argument("--limit", type=int, default=5)
    parser.add_argument("--image", default="agentbench-ltp-nova:local")
    parser.add_argument("--out", type=Path, default=Path("cases.jsonl"))
    args = parser.parse_args()

    root = args.agentbench_root.resolve()
    workbook = root / "data" / "lateralthinkingpuzzle" / f"{args.split}.xlsx"
    if not workbook.exists():
      raise SystemExit(f"AgentBench LTP workbook not found: {workbook}")

    rows = read_xlsx_rows(workbook)[: args.limit]
    commit = "unknown"
    head = root / ".git" / "HEAD"
    if head.exists():
        head_text = head.read_text(encoding="utf-8").strip()
        if head_text.startswith("ref: "):
            ref = root / ".git" / head_text.removeprefix("ref: ")
            if ref.exists():
                commit = ref.read_text(encoding="utf-8").strip()
        else:
            commit = head_text

    with args.out.open("w", encoding="utf-8") as handle:
        for idx, row in enumerate(rows, start=1):
            case_id = f"agentbench_ltp_{args.split}_{idx:03d}"
            answer_keys = split_keys(row.get("Answer keys", ""))
            story_keys = split_keys(row.get("Story keys", ""))
            case = {
                "schema_version": "case_v2",
                "id": case_id,
                "inputs": {
                    "prompt": build_prompt(row["story"]),
                    "story": row["story"],
                    "answer": row.get("answer", ""),
                    "story_keys": story_keys,
                    "answer_keys": answer_keys,
                },
                "resources": {
                    "workspace": {
                        "source": "container_image",
                        "image": args.image,
                        "workdir": "/bucephalus/workspace",
                    }
                },
                "metadata": {
                    "benchmark": "AgentBench",
                    "task": "lateralthinkingpuzzle",
                    "split": args.split,
                    "source_workbook": str(workbook),
                    "source_commit": commit,
                    "source_row": idx + 1,
                },
            }
            handle.write(json.dumps(case, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main()
