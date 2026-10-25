#!/usr/bin/env python3
"""Validate a SIM-R09 long-soak JSON report for the R10 rc gate."""
import argparse
import json
import pathlib
import shutil
import subprocess
import sys


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("report")
    parser.add_argument("--min-steady", type=int, default=3600)
    parser.add_argument("--min-idle", type=int, default=60)
    parser.add_argument("--max-rss-kib", type=int, default=10240)
    parser.add_argument("--require-commit", default="")
    parser.add_argument(
        "--allow-ancestor",
        action="store_true",
        help="allow the report commit to be an ancestor when only tests/tools/docs changed",
    )
    parser.add_argument("--output", default="")
    args = parser.parse_args()

    path = pathlib.Path(args.report)
    try:
        data = json.loads(path.read_text())
    except Exception as error:
        print(f"invalid report {path}: {error}", file=sys.stderr)
        return 1

    problems = []
    if data.get("mode") != "long":
        problems.append(f"mode is {data.get('mode')!r}, expected 'long'")
    if data.get("ok") is not True:
        problems.append("ok is not true")
    config = data.get("config") or {}
    steady = int(config.get("steady_seconds", 0))
    idle = int(config.get("idle_seconds", 0))
    if steady < args.min_steady:
        problems.append(f"steady_seconds {steady} < {args.min_steady}")
    if idle < args.min_idle:
        problems.append(f"idle_seconds {idle} < {args.min_idle}")
    samples = data.get("samples") or {}
    for name in ("baseline", "midpoint", "end", "idle"):
        sample = samples.get(name)
        if not sample:
            problems.append(f"missing {name} sample")
            continue
        if sample.get("threads") is None or sample.get("fds") is None:
            problems.append(f"{name} sample lacks thread/FD counts")
        containers = sample.get("containers") or {}
        for key in ("pending_requests", "held_frames", "frame_transactions"):
            if containers.get(key, 0) != 0:
                problems.append(f"{name}.containers.{key} = {containers.get(key)}")
    early = samples.get("early")
    if early:
        if early.get("threads") is None or early.get("fds") is None:
            problems.append("early sample lacks thread/FD counts")
    growth = data.get("rss_growth_kib")
    if growth is None:
        problems.append("rss_growth_kib is missing")
    elif growth > args.max_rss_kib:
        problems.append(f"RSS growth {growth} KiB > {args.max_rss_kib} KiB")
    ancestor_note = None
    if args.require_commit:
        commit = data.get("commit")
        if commit != args.require_commit:
            if not args.allow_ancestor or not commit:
                problems.append(f"commit {commit!r} != {args.require_commit!r}")
            else:
                is_ancestor = subprocess.run(
                    ["git", "merge-base", "--is-ancestor", commit, args.require_commit],
                    capture_output=True,
                ).returncode == 0
                changed = []
                if is_ancestor:
                    changed = subprocess.check_output(
                        ["git", "diff", "--name-only", f"{commit}..{args.require_commit}"],
                        text=True,
                    ).splitlines()
                allowed = ("tests/", "tools/", "docs/", ".github/", "Cargo.lock")
                outside = [path for path in changed if not path.startswith(allowed)]
                if not is_ancestor:
                    problems.append(f"commit {commit!r} is not an ancestor of {args.require_commit!r}")
                elif outside:
                    problems.append(
                        "ancestor report differs outside tests/tools/docs: "
                        + ", ".join(sorted(outside))
                    )
                else:
                    ancestor_note = {
                        "report_commit": commit,
                        "head_commit": args.require_commit,
                        "changed": changed,
                    }
    steps = data.get("steps") or []
    if not any(step.get("name") == "long-soak" and step.get("status") == "passed" for step in steps):
        problems.append("long-soak step is not passed")

    if problems:
        print(f"long report rejected: {path}", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        return 1

    if args.output:
        destination = pathlib.Path(args.output)
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, destination)
    summary = {
        "ok": True,
        "commit": data.get("commit"),
        "steady_seconds": steady,
        "idle_seconds": idle,
        "rss_growth_kib": growth,
        "transactions": (samples.get("end") or {}).get("transactions"),
    }
    if ancestor_note:
        summary["ancestor"] = ancestor_note
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
