# Soak reports

R09 evidence lives here. New runs sample baseline (after warmup), early
(steady/6 ≈ 10 min), midpoint (steady/2 ≈ 30 min), end (60 min) and idle. The
first 60-minute evidence run used 5/30/60-minute samples, which is stricter at
the start; its report is converted from the raw log.

- Core commit used to build the test binary
- platform / arch / build profile
- warmup, steady and idle durations
- RSS growth against the post-warmup baseline
- thread and FD counts at baseline/end/idle
- container counts (pending, held, frame transactions, ACK window, timings)
- transaction count and package/reconnect counts when available

New runs should set `MDC_SOAK_REPORT` (or use `tools/gates.sh long`) and write
`long-soak.json` plus its raw log. For a run started directly with
`cargo test`, convert the raw log:

```sh
tools/soak-log-report.py /tmp/mdc-r09-long.log \
  --commit "$(git rev-parse HEAD)" \
  --warmup 300 --steady 3600 --idle 60 \
  --output docs/soak-reports/r09-<commit>.json
tools/validate-long-report.py docs/soak-reports/r09-<commit>.json \
  --require-commit <commit>
```

Raw logs are small enough to keep beside the report. A report with missing
samples, a failed test result, a shorter steady window or RSS growth above the
10 MiB budget is rejected.

Recorded report: `r09-4311e15.json` / `r09-4311e15.log` (60 minutes, RSS growth
0 KiB, 2,604,243 transactions, threads 2 and FDs 4 unchanged).
