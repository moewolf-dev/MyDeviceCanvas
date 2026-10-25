#!/usr/bin/env bash
# SIM-R10: repeatable simulator delivery gates.
#
#   tools/gates.sh quick    # fmt/clippy/Rust/C++/Node
#   tools/gates.sh cross    # fault matrix + C++ peer + native smoke
#   tools/gates.sh long     # 60-minute R09 soak (configurable)
#   tools/gates.sh rc       # clean tree + quick + cross + long + App pin/native
#   tools/gates.sh all      # quick + cross + long
#
# Reports land in target/gates/<timestamp>-<mode>/report.json (override with
# MDC_GATE_REPORT_DIR).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

MODE="${1:-quick}"
case "$MODE" in
  quick|cross|long|rc|all) ;;
  *) echo "usage: $0 {quick|cross|long|rc|all}" >&2; exit 2 ;;
esac

CARGO_BIN="${CARGO_BIN:-cargo}"
CXX="${CXX:-c++}"
NODE_BIN="${NODE_BIN:-node}"
NPM_BIN="${NPM_BIN:-npm}"
export CARGO_BIN CXX NODE_BIN NPM_BIN
REPORT_DIR="${MDC_GATE_REPORT_DIR:-$ROOT/target/gates}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_DIR="$REPORT_DIR/$STAMP-$MODE"
mkdir -p "$RUN_DIR"
STEPS_TSV="$RUN_DIR/steps.tsv"
: >"$STEPS_TSV"
START_ISO="$(date -u +%FT%TZ)"
START_EPOCH="$(date +%s)"
FAILED=0

run_step() {
  local name="$1"
  shift
  local log="$RUN_DIR/$name.log"
  local started ended status
  printf 'gate: %-24s ' "$name"
  started="$(date +%s)"
  set +e
  "$@" >"$log" 2>&1
  status=$?
  set -e
  ended="$(date +%s)"
  if [[ $status -eq 0 ]]; then
    printf 'ok (%ss)\n' "$((ended - started))"
  else
    printf 'FAIL (%ss) %s\n' "$((ended - started))" "$log"
    FAILED=1
  fi
  printf '%s\t%s\t%s\t%s\n' "$name" "$status" "$((ended - started))" "$log" >>"$STEPS_TSV"
}

cpp_tests() {
  "$CXX" -std=c++17 -I runtime/esp32/include \
    runtime/esp32/src/parser.cpp runtime/esp32/src/frame_store.cpp \
    runtime/esp32/src/sim_display.cpp runtime/esp32/src/dispatcher.cpp \
    runtime/esp32/src/cbor_control.cpp runtime/esp32/tests/parser_test.cpp \
    -o /tmp/mdc_gate_parser
  /tmp/mdc_gate_parser
  "$CXX" -std=c++17 -I runtime/esp32/include \
    runtime/esp32/src/cbor_control.cpp runtime/esp32/tests/cbor_control_test.cpp \
    -o /tmp/mdc_gate_cbor
  /tmp/mdc_gate_cbor
  "$CXX" -std=c++17 -I runtime/esp32/include \
    runtime/esp32/src/sim_display.cpp runtime/esp32/tests/adapter_test.cpp \
    -o /tmp/mdc_gate_adapter
  /tmp/mdc_gate_adapter
  "$CXX" -std=c++17 -I runtime/esp32/include \
    runtime/esp32/src/frame_store.cpp runtime/esp32/src/sim_display.cpp \
    runtime/esp32/src/dispatcher.cpp runtime/esp32/src/cbor_control.cpp \
    runtime/esp32/tests/package_test.cpp -o /tmp/mdc_gate_package
  /tmp/mdc_gate_package
  "$CXX" -std=c++17 -I runtime/linux/include -I runtime/esp32/include \
    runtime/esp32/src/parser.cpp runtime/esp32/src/frame_store.cpp \
    runtime/esp32/src/sim_display.cpp runtime/esp32/src/dispatcher.cpp \
    runtime/esp32/src/cbor_control.cpp runtime/linux/src/linux_runtime.cpp \
    runtime/linux/tests/linux_runtime_test.cpp -o /tmp/mdc_gate_linux
  /tmp/mdc_gate_linux
  "$CXX" -std=c++17 runtime/esp32/tests/vector_test.cpp -o /tmp/mdc_gate_vectors
  /tmp/mdc_gate_vectors protocol/vectors
}

build_native_debug() {
  "$CARGO_BIN" build -p mdc-node-native -p mdc-sim-peer
  if [[ -f target/debug/libmdc_node_native.dylib ]]; then
    cp target/debug/libmdc_node_native.dylib sdk/node/mdc-node-native.node
  elif [[ -f target/debug/libmdc_node_native.so ]]; then
    cp target/debug/libmdc_node_native.so sdk/node/mdc-node-native.node
  else
    echo "native artifact missing" >&2
    return 1
  fi
}

node_smoke() {
  local major
  major="$("$NODE_BIN" -p 'process.versions.node.split(".")[0]')"
  if [[ "$major" != 20 ]]; then
    echo "Node $major is not the declared Node 20 line; set NODE_BIN" >&2
    return 3
  fi
  rm -f sdk/node/mdc-node-native.node
  (cd sdk/node && "$NODE_BIN" --test index.test.js)
  build_native_debug
  "$NODE_BIN" --test sdk/node/native-smoke.test.js
  rm -f sdk/node/mdc-node-native.node
}

board_validation() {
  local board
  for board in \
    boards/sim/esp32-jc3248w535-sim/board.yaml \
    boards/sim/linux-virt/board.yaml \
    boards/linux/pi4-hdmi/board.yaml \
    boards/linux/waveshare-eink/board.yaml \
    boards/esp32/test-valid/board.yaml \
    boards/arduino/uno-r4-ili9341/board.yaml \
    boards/arduino/uno-tft/board.yaml \
    boards/rockchip/rk3399-nanopc-t4/board.yaml \
    boards/nvidia/jetson-orin-nano/board.yaml \
    boards/esp32/esp32s3-box-3/board.yaml; do
    python3 tools/validate_board.py "$board"
  done
}

fault_matrix() {
  "$CARGO_BIN" test -p mdc-sim-peer --test faults
}

cxx_peer() {
  tools/cxx-peer-check.sh
}

long_soak() {
  MDC_SOAK_REPORT="$RUN_DIR/long-soak.json" MDC_LONG_SOAK=1 \
    MDC_SOAK_WARMUP_SECS="${MDC_SOAK_WARMUP_SECS:-300}" \
    MDC_SOAK_SECS="${MDC_SOAK_SECS:-3600}" \
    MDC_SOAK_IDLE_SECS="${MDC_SOAK_IDLE_SECS:-60}" \
    MDC_RSS_BUDGET_KIB="${MDC_RSS_BUDGET_KIB:-10240}" \
    "$CARGO_BIN" test -p mdc-memory-tests --test long_soak -- --nocapture
}

validate_long_report() {
  python3 tools/validate-long-report.py "$MDC_LONG_REPORT" \
    --require-commit "$(git rev-parse HEAD)" --allow-ancestor \
    --output "$RUN_DIR/long-soak.json"
}

clean_tree() {
  test -z "$(git status --porcelain)"
}

app_pin_matches() {
  local expected actual
  expected="$(git rev-parse HEAD)"
  actual="$(grep -E "CORE_PIN_SHA = '[0-9a-f]+'" ../moetuber/src/adapters/core/native.ts \
    | head -1 | sed -E "s/.*'([0-9a-f]+)'.*/\1/")"
  echo "core=$expected"
  echo "app =$actual"
  test "$expected" = "$actual"
}

app_check() {
  (cd ../moetuber && PATH="$(dirname "$NODE_BIN"):$PATH" "$NPM_BIN" run check)
}

app_native() {
  (cd ../moetuber && PATH="$(dirname "$NODE_BIN"):$PATH" "$NPM_BIN" run build:native \
    && PATH="$(dirname "$NODE_BIN"):$PATH" "$NPM_BIN" run verify:native)
}

release_native() {
  "$CARGO_BIN" build --release -p mdc-node-native
  local artifact="target/release/libmdc_node_native.dylib"
  [[ -f "$artifact" ]] || artifact="target/release/libmdc_node_native.so"
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$artifact" | tee "$RUN_DIR/native.sha256"
  else
    sha256sum "$artifact" | tee "$RUN_DIR/native.sha256"
  fi
}

if [[ "$MODE" == "rc" ]]; then
  run_step clean-tree clean_tree
  if [[ $FAILED -ne 0 ]]; then
    echo "release candidate requires a clean working tree" >&2
    exit 1
  fi
fi

if [[ "$MODE" == "quick" || "$MODE" == "all" || "$MODE" == "rc" ]]; then
  run_step fmt "$CARGO_BIN" fmt --all -- --check
  run_step clippy "$CARGO_BIN" clippy --workspace --all-targets -- -D warnings
  run_step rust-tests "$CARGO_BIN" test --workspace
  run_step python-tools python3 -m unittest discover -s tools -p 'test_*.py'
  run_step board-validation board_validation
  run_step cpp-tests cpp_tests
  run_step node-sdk node_smoke
fi

if [[ "$MODE" == "cross" || "$MODE" == "all" || "$MODE" == "rc" ]]; then
  run_step fault-matrix fault_matrix
  run_step cxx-peer cxx_peer
  run_step native-smoke node_smoke
fi

if [[ "$MODE" == "long" || "$MODE" == "all" ]]; then
  run_step long-soak long_soak
elif [[ "$MODE" == "rc" ]]; then
  if [[ -n "${MDC_LONG_REPORT:-}" ]]; then
    run_step long-soak validate_long_report
  else
    run_step long-soak long_soak
  fi
fi

if [[ "$MODE" == "rc" ]]; then
  run_step release-native release_native
  run_step app-check app_check
  run_step app-native app_native
  run_step app-pin app_pin_matches
fi

python3 - "$RUN_DIR" "$MODE" "$START_ISO" "$START_EPOCH" "$STEPS_TSV" <<'PY'
import datetime
import json
import os
import pathlib
import platform
import re
import subprocess
import sys

run_dir, mode, started_iso, start_epoch, steps_tsv = sys.argv[1:6]
run_dir = pathlib.Path(run_dir)


def version(command):
    try:
        return subprocess.check_output(command, text=True, stderr=subprocess.DEVNULL).strip()
    except Exception:
        return None


def git(*args):
    try:
        return subprocess.check_output(["git", *args], text=True, stderr=subprocess.DEVNULL).strip()
    except Exception:
        return None


steps = []
for line in pathlib.Path(steps_tsv).read_text().splitlines():
    name, status, seconds, log = line.split("\t")
    steps.append({
        "name": name,
        "status": "passed" if status == "0" else "failed",
        "seconds": int(seconds),
        "log": os.path.relpath(log, run_dir),
    })

peak = {}
skipped = []
long = None
long_path = run_dir / "long-soak.json"
if long_path.exists():
    long = json.loads(long_path.read_text())
    growth = long.get("rss_growth_kib")
    if growth is not None:
        peak["rss_growth_kib"] = growth
    end = (long.get("samples") or {}).get("end") or {}
    if end.get("transactions") is not None:
        peak["transactions"] = end["transactions"]

for step in steps:
    text = (run_dir / step["log"]).read_text(errors="replace") if (run_dir / step["log"]).exists() else ""
    match = re.search(r"RSS growth (\d+) KiB", text)
    if match:
        peak["rss_growth_kib"] = int(match.group(1))
    match = re.search(r"transactions: (\d+)", text)
    if match:
        peak["transactions"] = int(match.group(1))
    if "skipping" in text:
        skipped.append(step["name"])

report = {
    "mode": mode,
    "started_at": started_iso,
    "finished_at": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "duration_seconds": int(datetime.datetime.now().timestamp() - int(start_epoch)),
    "ok": all(step["status"] == "passed" for step in steps),
    "repo": {
        "commit": git("rev-parse", "HEAD"),
        "dirty": bool(git("status", "--porcelain")),
    },
    "toolchain": {
        "rustc": version(["rustc", "--version"]),
        "cargo": version([os.environ.get("CARGO_BIN", "cargo"), "--version"]),
        "node": version([os.environ.get("NODE_BIN", "node"), "--version"]),
        "python": platform.python_version(),
    },
    "platform": {
        "system": platform.system(),
        "release": platform.release(),
        "machine": platform.machine(),
    },
    "limits": {
        "frame_bytes": 1024 * 1024,
        "control_bytes": 4096,
        "package_bytes": 16 * 1024 * 1024,
        "poll_bytes": 64 * 1024,
        "poll_packets": 32,
        "rss_growth_kib": 10 * 1024,
    },
    "peak": peak or None,
    "long": long,
    "skipped_steps": skipped,
    "steps": steps,
}
(run_dir / "report.json").write_text(json.dumps(report, indent=2) + "\n")
print(json.dumps({key: report[key] for key in ("mode", "ok", "duration_seconds", "peak")}, indent=2))
print(f"report: {run_dir / 'report.json'}")
sys.exit(0 if report["ok"] else 1)
PY

if [[ $FAILED -ne 0 ]]; then
  echo "gate failed; inspect $RUN_DIR" >&2
  exit 1
fi
echo "gate passed: $RUN_DIR"
