"""Print benchmark results from a JSON lines file as a Markdown table."""

import json
import sys


def fmt(value, unit):
    if unit == "B/s":
        return f"{value / 1024:.1f} KiB/s"
    return f"{value:.1f} {unit}"


print("| Chip | Probe | Protocol | Speed (kHz) | Metric | Size (B) | Median | p10 - p90 |")
print("|---|---|---|---|---|---|---|---|")
with open(sys.argv[1]) as f:
    for line in f:
        r = json.loads(line)
        u = r["unit"]
        print(
            f"| {r['chip']} | {r['probe']} | {r['protocol'] or '-'} | {r['speed_khz']} "
            f"| {r['metric']} | {r['size']} | {fmt(r['median'], u)} "
            f"| {fmt(r['p10'], u)} - {fmt(r['p90'], u)} |"
        )
