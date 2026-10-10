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
python3 tools/validate_board.py boards/linux/pi4-hdmi/board.yaml
python3 tools/validate_board.py boards/linux/waveshare-eink/board.yaml
python3 tools/validate_board.py boards/esp32/test-valid/board.yaml
python3 -m unittest discover -s tools -p 'test_*.py'

c++ -std=c++17 -I runtime/esp32/include \
  runtime/esp32/src/parser.cpp \
  runtime/esp32/src/frame_store.cpp \
  runtime/esp32/src/sim_display.cpp \
  runtime/esp32/src/dispatcher.cpp \
  runtime/esp32/src/cbor_control.cpp \
  runtime/esp32/tests/parser_test.cpp \
  -o /tmp/mdc_runtime_tests
/tmp/mdc_runtime_tests
c++ -std=c++17 -I runtime/esp32/include \
  runtime/esp32/src/cbor_control.cpp \
  runtime/esp32/tests/cbor_control_test.cpp \
  -o /tmp/mdc_cbor_control_tests
/tmp/mdc_cbor_control_tests
c++ -std=c++17 -I runtime/esp32/include \
  runtime/esp32/src/sim_display.cpp \
  runtime/esp32/tests/adapter_test.cpp \
  -o /tmp/mdc_adapter_tests
/tmp/mdc_adapter_tests
if [[ -d runtime/linux ]]; then
  c++ -std=c++17 -I runtime/linux/include -I runtime/esp32/include \
    runtime/esp32/src/parser.cpp runtime/esp32/src/frame_store.cpp \
    runtime/esp32/src/sim_display.cpp runtime/esp32/src/dispatcher.cpp \
    runtime/esp32/src/cbor_control.cpp \
    runtime/linux/src/linux_runtime.cpp runtime/linux/tests/linux_runtime_test.cpp \
    -o /tmp/mdc_linux_runtime_tests
  /tmp/mdc_linux_runtime_tests
fi

if [[ -d sdk/node ]]; then
  (cd sdk/node && node --test index.test.js)
fi
if [[ -f examples/electron-demo/demo-logic.test.js ]]; then
  node --test examples/electron-demo/demo-logic.test.js
fi
if command -v swiftc >/dev/null 2>&1; then
  cargo build -p mdc_c
  swiftc -import-objc-header sdk/c/include/mdc.h \
    -L target/debug -lmdc_c \
    -Xlinker -rpath -Xlinker "$ROOT/target/debug" \
    sdk/swift/Sources/MdcSession.swift sdk/swift/main.swift \
    -o /tmp/mdc-swift-hello
  /tmp/mdc-swift-hello
fi

echo "check.sh: all host/sim gates passed"
