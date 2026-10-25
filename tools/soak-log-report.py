#!/usr/bin/env python3
"""Convert a raw R09 `long_soak` test log into the JSON report schema.

This is for runs started directly with `cargo test` (for example the first
60-minute evidence run). New runs should set `MDC_SOAK_REPORT` so the test writes
the JSON itself. The converter only accepts logs that contain every sample and a
passing test result.
"""
import argparse
import json
import pathlib
import re
import subprocess
import sys

SAMPLE_RE = re.compile(
    r"R09 (baseline|midpoint|end|idle) Sample \{ rss_kib: (Some\((\d+)\)|None), "
    r"threads: (Some\((\d+)\)|None), fds: (Some\((\d+)\)|None), "
    r"counts: ContainerCounts \{ (.*?) \}, transactions: (\d+) \}"
)
GROWTH_RE = re.compile(r"R09 RSS growth (\d+) KiB \(budget (\d+) KiB\)")
COUNT_RE = re.compile(r"(\w+): (\d+)")


def parse_sample(match):
    counts = {key: int(value) for key, value in COUNT_RE.findall(match.group(8))}
    return {
        "rss_kib": int(match.group(3)) if match.group(3) else None,
        "threads": int(match.group(5)) if match.group(5) else None,
        "fds": int(match.group(7)) if match.group(7) else None,
        "transactions": int(match.group(9)),
        "containers": counts,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("log")
    parser.add_argument("--commit", default="")
    parser.add_argument("--warmup", type=int, default=300)
    parser.add_argument("--steady", type=int, default=3600)
    parser.add_argument("--idle", type=int, default=60)
    parser.add_argument("--budget-kib", type=int, default=10240)
    parser.add_argument("--output", required=True)
    parser.add_argument("--source", default="raw long_soak log")
    args = parser.parse_args()

    log_path = pathlib.Path(args.log)
    text = log_path.read_text(errors="replace")
    samples = {}
    for match in SAMPLE_RE.finditer(text):
        samples[match.group(1)] = parse_sample(match)
    missing = [name for name in ("baseline", "midpoint", "end", "idle") if name not in samples]
    if missing:
        print(f"rejecting {log_path}: missing samples: {', '.join(missing)}", file=sys.stderr)
        return 1
    if "test result: ok." not in text:
        print(f"rejecting {log_path}: test did not pass", file=sys.stderr)
        return 1
    growth_match = GROWTH_RE.search(text)
    if not growth_match:
        print(f"rejecting {log_path}: RSS growth line missing", file=sys.stderr)
        return 1
    growth = int(growth_match.group(1))
    budget = int(growth_match.group(2))
    if growth > budget:
        print(f"rejecting {log_path}: RSS growth {growth} KiB > {budget} KiB", file=sys.stderr)
        return 1

    commit = args.commit
    if not commit:
        commit = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()

    report = {
        "mode": "long",
        "source": args.source,
        "log": str(log_path),
        "commit": commit,
        "ok": True,
        "config": {
            "warmup_seconds": args.warmup,
            "steady_seconds": args.steady,
            "idle_seconds": args.idle,
            "rss_budget_kib": args.budget_kib,
        },
        "samples": samples,
        "rss_growth_kib": growth,
        "steps": [{"name": "long-soak", "status": "passed"}],
    }
    output = pathlib.Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2) + "\n")
    print(
        json.dumps(
            {
                "ok": True,
                "commit": commit,
                "rss_growth_kib": growth,
                "transactions": samples["end"]["transactions"],
                "report": str(output),
            },
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
