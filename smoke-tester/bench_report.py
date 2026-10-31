"""Merge smoke tester benchmark results and compare them with a baseline.

Usage: bench_report.py RESULTS_DIR OUT_DIR [--baseline FILE --baseline-sha SHA] [--run-url URL]

RESULTS_DIR holds one `bench-<runner>/bench.jsonl` file for each runner. The script writes:

- `results.jsonl`: all records, with the runner name added.
- `summary.md`: all records, with the change from the baseline.
- `comment.md`: only the changes beyond the noise threshold. The script writes it only if such
  changes exist.
- `clear.md`: the PR comment text if no changes exist.

A result with one sample has no spread. Its change shows in the summary, but is never significant.
"""

import argparse
import json
from pathlib import Path

# A change is significant if it is larger than this fraction, and larger than the baseline spread.
THRESHOLD = 0.10

KEY = ("runner", "chip", "probe", "protocol", "speed_khz", "metric", "size")


def key(record):
    return tuple(record.get(k) for k in KEY)


def read_jsonl(path):
    records = []
    for line in path.read_text().splitlines():
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(record, dict):
            records.append(record)
    return records


def compare(current, baseline):
    """Return the relative change and "better", "worse" or None if not significant."""
    if baseline is None or not baseline["median"]:
        return None, None
    change = current["median"] / baseline["median"] - 1
    spread = abs(baseline["p90"] - baseline["p10"]) / baseline["median"]
    if abs(change) <= max(THRESHOLD, spread) or min(current["samples"], baseline["samples"]) < 2:
        return change, None
    better = (change > 0) == (current["improves"] == "up")
    return change, "better" if better else "worse"


def fmt_value(value, unit):
    if unit == "B/s":
        return f"{value / 1024:.1f} KiB/s"
    return f"{value:.1f} {unit}"


def cell(value):
    return str(value).replace("|", "\\|").replace("\n", " ")


def row(record, change, outcome):
    unit = record["unit"]
    if change is None:
        change_text = "-"
    else:
        change_text = f"{change * 100:+.1f} %"
        if outcome:
            change_text += f" ({outcome})"
    cells = [
        record["runner"],
        record["chip"],
        record["probe"],
        record.get("protocol") or "-",
        record["speed_khz"],
        record["metric"],
        record["size"] or "-",
        fmt_value(record["median"], unit),
        f"{fmt_value(record['p10'], unit)} - {fmt_value(record['p90'], unit)}",
        change_text,
    ]
    return "| " + " | ".join(cell(c) for c in cells) + " |"


HEADER = (
    "| Runner | Chip | Probe | Protocol | Speed (kHz) | Metric | Size (B) | Median | p10 - p90 "
    "| Change |\n|---|---|---|---|---|---|---|---|---|---|"
)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("results", type=Path)
    parser.add_argument("out", type=Path)
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--baseline-sha")
    parser.add_argument("--run-url")
    args = parser.parse_args()
    if bool(args.baseline) != bool(args.baseline_sha):
        parser.error("--baseline and --baseline-sha must be given together")

    records = []
    for path in sorted(args.results.glob("bench-*/bench.jsonl")):
        runner = path.parent.name.removeprefix("bench-")
        for record in read_jsonl(path):
            record["runner"] = runner
            records.append(record)

    baseline = {}
    if args.baseline:
        baseline = {key(r): r for r in read_jsonl(args.baseline)}

    args.out.mkdir(parents=True, exist_ok=True)
    with open(args.out / "results.jsonl", "w") as f:
        for record in records:
            f.write(json.dumps(record) + "\n")

    if args.baseline:
        compared = f"Compared with {args.baseline_sha[:9]}."
    else:
        compared = "No baseline found."

    rows = []
    changed = []
    for record in records:
        change, outcome = compare(record, baseline.get(key(record)))
        rows.append(row(record, change, outcome))
        if outcome:
            changed.append(rows[-1])

    summary = ["## Probe benchmark", "", compared, ""]
    summary += [HEADER, *rows] if rows else ["No results."]
    (args.out / "summary.md").write_text("\n".join(summary) + "\n")

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
    (args.out / name).write_text("\n".join(comment) + "\n")


if __name__ == "__main__":
    main()
