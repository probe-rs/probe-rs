"""Merge the backfill results into a bench-data worktree.

Usage: publish.py ARTIFACTS DATA

ARTIFACTS holds `<run>/bench-<runner>[-<part>]-<sha>/bench.jsonl` files. DATA is a worktree of the
bench-data branch. The script adds the records that a file does not have yet, and makes one commit
for each master commit. The benchmark page finds the results by the commit message, and sorts them
by the commit date, so the commit gets the date of the master commit.

Before #4569, the speed is the speed before attach, which is 0 for J-Link. The script then uses the
speed of the live data.
"""

import json
import os
import re
import subprocess
import sys
from pathlib import Path

KEY = ("runner", "chip", "probe", "protocol", "speed_khz", "metric", "size")
PROBE = ("runner", "chip", "probe", "protocol")
ARTIFACT = re.compile(r"bench-(.+?)-(?:(?:arm|esp-rp|all)-)?([0-9a-f]{40})")
MESSAGE = re.compile(r"^Add benchmark results for ([0-9a-f]{40})", re.MULTILINE)


def load(path: Path) -> list[dict]:
    if not path.is_file():
        return []
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def key(record: dict, fields=KEY) -> tuple:
    return tuple(record.get(k) for k in fields)


def git(data: Path, *args: str, env: dict | None = None) -> str:
    return subprocess.run(
        ["git", "-C", str(data), *args], check=True, capture_output=True, text=True, env=env
    ).stdout


def main() -> None:
    artifacts, data = Path(sys.argv[1]), Path(sys.argv[2])
    repo = os.environ["GITHUB_REPOSITORY"]

    results: dict[str, list[dict]] = {}
    for path in sorted(artifacts.glob("*/bench-*/bench.jsonl")):
        match = ARTIFACT.fullmatch(path.parent.name)
        if not match:
            print(f"::warning::Unknown artifact {path.parent.name}")
            continue
        runner, sha = match.groups()
        for record in load(path):
            record["runner"] = runner
            results.setdefault(sha, []).append(record)

    live = [r for path in data.glob("*.jsonl") for r in load(path)]
    live_keys = {key(r) for r in live}
    speeds: dict[tuple, set] = {}
    for record in live:
        speeds.setdefault(key(record, PROBE), set()).add(record.get("speed_khz"))

    marked = set(MESSAGE.findall(git(data, "log", "--format=%B")))
    dates = {
        sha: subprocess.run(
            ["gh", "api", f"repos/{repo}/commits/{sha}", "-q", ".commit.committer.date"],
            check=True, capture_output=True, text=True,
        ).stdout.strip()
        for sha in results
    }

    for sha in sorted(results, key=dates.__getitem__):
        path = data / f"{sha}.jsonl"
        records = load(path)
        known = {key(r) for r in records}
        added = 0
        for record in results[sha]:
            speed = speeds.get(key(record, PROBE), set())
            if record.get("speed_khz") == 0 and len(speed) == 1:
                record["speed_khz"] = next(iter(speed))
            if key(record) not in live_keys:
                print(f"::warning::{sha[:9]}: key not in the live data: {key(record)}")
            if key(record) not in known:
                known.add(key(record))
                records.append(record)
                added += 1
        print(f"{sha[:9]}: {added} new results, {len(records)} in all")
        if added == 0 and sha in marked:
            continue
        path.write_text("".join(json.dumps(r) + "\n" for r in records))
        git(data, "add", path.name)
        env = {**os.environ, "GIT_AUTHOR_DATE": dates[sha], "GIT_COMMITTER_DATE": dates[sha]}
        git(data, "commit", "-q", "--allow-empty", "-m", f"Add benchmark results for {sha}", env=env)


if __name__ == "__main__":
    main()
