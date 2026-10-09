#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

cargo fmt --all -- --check
cargo test --workspace
cargo test --workspace --all-features
cargo clippy --workspace --all-targets -- -D warnings

python3 tools/validate_board.py boards/sim/esp32-jc3248w535-sim/board.yaml
python3 tools/validate_board.py boards/sim/linux-virt/board.yaml
python3 tools/validate_board.py boards/esp32/test-valid/board.yaml
python3 -m unittest discover -s tools -p 'test_*.py'

CPP_SRCS=(
  runtime/esp32/src/parser.cpp
  runtime/esp32/src/frame_store.cpp
)
if [[ -f runtime/esp32/src/sim_display.cpp ]]; then
  CPP_SRCS+=(runtime/esp32/src/sim_display.cpp)
fi
c++ -std=c++17 -I runtime/esp32/include "${CPP_SRCS[@]}" \
  runtime/esp32/tests/parser_test.cpp -o /tmp/mdc_runtime_tests
/tmp/mdc_runtime_tests

if [[ -d sdk/node ]]; then
  (cd sdk/node && node --test index.test.js)
fi

echo "check.sh: all host/sim gates passed"
