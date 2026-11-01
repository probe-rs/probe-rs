"""Merge smoke tester benchmark results and compare them with a baseline.

Usage: bench_report.py RESULTS OUT_DIR [--baseline FILE ...] [--run-url URL]

RESULTS is a directory with one `bench-<runner>/bench.jsonl` file for each runner, or a
`results.jsonl` file from an earlier report.

Each baseline file is `<sha>.jsonl` from the `bench-data` branch, or `baseline.jsonl` from an
earlier report. Give the files nearest commit first. Each result uses the first file that has a
result for the same test.

The script writes:

- `results.jsonl`: all records, with the runner name added.
- `baseline.jsonl`: the baseline records, with their commit hash added.
- `summary.md`: all records, with the change from the baseline.
- `comment.md`: only the changes beyond the noise threshold. The script writes it only if such
  changes exist.
- `clear.md`: the PR comment text if no changes exist.

A result with one sample has no spread. Its change shows in the summary, but is never significant.

The records can come from PR code. Each table cell is a code span, so that the records cannot add
links, mentions or HTML to the comment.
"""

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any

# A change is significant if it is larger than this fraction, and larger than the baseline spread.
THRESHOLD = 0.10

KEY = ("runner", "chip", "probe", "protocol", "speed_khz", "metric", "size")

SHA = re.compile(r"[0-9a-f]{40}")

Record = dict[str, Any]
Key = tuple[str | int | float | None, ...]


def key(record: Record) -> Key:
    return tuple(record.get(k) for k in KEY)


def number(value: object) -> bool:
    # Also rejects bool, NaN, infinity and integers too large for a float.
    return isinstance(value, (int, float)) and not isinstance(value, bool) and abs(value) < 1e300


def valid(record: object) -> bool:
    return (
        isinstance(record, dict)
        # A key field must be hashable.
        and all(type(record.get(k)) in (str, int, float, type(None)) for k in KEY)
        and all(number(record.get(k)) for k in ("median", "p10", "p90"))
        and type(record.get("samples")) is int
        and isinstance(record.get("unit"), str)
    )


def is_sha(value: object) -> bool:
    return isinstance(value, str) and SHA.fullmatch(value) is not None


def read_jsonl(path: Path) -> list[Record]:
    records = []
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        try:
            record = json.loads(line)
        except (ValueError, RecursionError):
            continue
        if valid(record):
            records.append(record)
    return records


def read_results(path: Path) -> list[Record]:
    if path.is_file():
        return read_jsonl(path)
    records = []
    for file in sorted(path.glob("bench-*/bench.jsonl")):
        runner = file.parent.name.removeprefix("bench-")
        for record in read_jsonl(file):
            record["runner"] = runner
            records.append(record)
    return records


def read_baseline(paths: list[Path]) -> dict[Key, Record]:
    baseline: dict[Key, Record] = {}
    for path in paths:
        found = False
        for record in read_jsonl(path):
            if is_sha(path.stem):
                record["baseline_sha"] = path.stem
            if is_sha(record.get("baseline_sha")):
                found = True
                baseline.setdefault(key(record), record)
        if not found:
            print(f"warning: {path} has no baseline records with a commit hash", file=sys.stderr)
    return baseline


def compare(current: Record, baseline: Record | None) -> tuple[float | None, str | None]:
    """Return the relative change and "better", "worse" or None if not significant."""
    if baseline is None or not baseline["median"]:
        return None, None
    change = current["median"] / baseline["median"] - 1
    spread = abs(baseline["p90"] - baseline["p10"]) / baseline["median"]
    if abs(change) <= max(THRESHOLD, spread) or min(current["samples"], baseline["samples"]) < 2:
        return change, None
    # Throughput gets better upward, latency downward.
    better = (change > 0) == (current["unit"] == "B/s")
    return change, "better" if better else "worse"


def fmt_value(value: float, unit: str) -> str:
    if unit == "B/s":
        return f"{value / 1024:.1f} KiB/s"
    return f"{value:.1f} {unit}"


def cell(value: object) -> str:
    # Without backslashes, an input cannot cancel the escape of a pipe.
    text = " ".join(str(value).replace("`", "'").replace("\\", "/").split()).replace("|", "\\|")
    return f"`{text}`" if text else ""


def row(record: Record, change: float | None, outcome: str | None) -> str:
    unit = record["unit"]
    if change is None:
        change_text = "-"
    else:
        change_text = f"{change * 100:+.1f} %"
        if outcome:
            change_text += f" ({outcome})"
    cells = [
        record.get("runner"),
        record.get("chip"),
        record.get("probe"),
        record.get("protocol") or "-",
        record.get("speed_khz"),
        record.get("metric"),
        record.get("size") or "-",
        fmt_value(record["median"], unit),
        f"{fmt_value(record['p10'], unit)} - {fmt_value(record['p90'], unit)}",
        change_text,
    ]
    return "| " + " | ".join(cell(c) for c in cells) + " |"


HEADER = (
    "| Runner | Chip | Probe | Protocol | Speed (kHz) | Metric | Size (B) | Median | p10 - p90 "
    "| Change |\n|---|---|---|---|---|---|---|---|---|---|"
)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("results", type=Path)
    parser.add_argument("out", type=Path)
    parser.add_argument("--baseline", type=Path, action="append", default=[])
    parser.add_argument("--run-url")
    args = parser.parse_args()

    records = read_results(args.results)
    baseline = read_baseline(args.baseline)

    args.out.mkdir(parents=True, exist_ok=True)
    with open(args.out / "results.jsonl", "w", encoding="utf-8") as f:
        f.writelines(json.dumps(record) + "\n" for record in records)
    with open(args.out / "baseline.jsonl", "w", encoding="utf-8") as f:
        f.writelines(json.dumps(record) + "\n" for record in baseline.values())

    used = {key(record) for record in records}
    shas = dict.fromkeys(b["baseline_sha"][:9] for k, b in baseline.items() if k in used)
    compared = f"Compared with {', '.join(shas)}." if shas else "No baseline found."

    rows = []
    changed = []
    for record in records:
        change, outcome = compare(record, baseline.get(key(record)))
        rows.append(row(record, change, outcome))
        if outcome:
            changed.append(rows[-1])

    summary = ["## Probe benchmark", "", compared, ""]
    summary += [HEADER, *rows] if rows else ["No results."]
    # The records can hold lone surrogates, which UTF-8 cannot encode.
    (args.out / "summary.md").write_text(
        "\n".join(summary) + "\n", encoding="utf-8", errors="replace"
    )

    if changed:
        comment = [
            "## Probe benchmark",
            "",
            f"{compared} Results that changed by more than the noise threshold: {len(changed)}.",
            "",
            HEADER,
            *changed,
        ]
        name = "comment.md"
    elif records:
        comment = [
            "## Probe benchmark",
            "",
            f"{compared} No results changed by more than the noise threshold.",
        ]
        name = "clear.md"
    else:
        comment = ["## Probe benchmark", "", "No results."]
        name = "clear.md"
    if args.run_url:
        comment += ["", f"[All results]({args.run_url})"]
    (args.out / name).write_text("\n".join(comment) + "\n", encoding="utf-8", errors="replace")


if __name__ == "__main__":
    main()
