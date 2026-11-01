"""Print the build and test matrices for $GITHUB_OUTPUT.

Each line of commits.txt is `<sha> [<part>,...] [# comment]`. The parts are keys of setups.json.
Without parts, the line runs all parts. A board that fails makes only its part fail, so a later
batch can run only that part.
"""

import json
import re
from pathlib import Path

SHA = re.compile(r"[0-9a-f]{40}")

setups = json.loads(Path("backfill/setups.json").read_text())
commits = []
jobs = []
for line in Path("backfill/commits.txt").read_text().splitlines():
    fields = line.split("#", 1)[0].split()
    if not fields or not SHA.fullmatch(fields[0]):
        continue
    sha = fields[0]
    parts = fields[1].split(",") if len(fields) > 1 else list(setups)
    for part in parts:
        if part not in setups:
            raise SystemExit(f"{sha}: unknown part {part}")
    commits.append(sha)
    jobs += [{"sha": sha, "part": part, **setups[part]} for part in parts]

print(f"commits={json.dumps(commits)}")
print(f"jobs={json.dumps(jobs)}")
