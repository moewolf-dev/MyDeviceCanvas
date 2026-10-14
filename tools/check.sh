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
python3 tools/validate_board.py boards/arduino/uno-r4-ili9341/board.yaml
python3 tools/validate_board.py boards/arduino/uno-tft/board.yaml
python3 tools/validate_board.py boards/rockchip/rk3399-nanopc-t4/board.yaml
python3 tools/validate_board.py boards/nvidia/jetson-orin-nano/board.yaml
python3 tools/validate_board.py boards/esp32/esp32s3-box-3/board.yaml
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
c++ -std=c++17 runtime/esp32/tests/vector_test.cpp -o /tmp/mdc_vector_tests
/tmp/mdc_vector_tests protocol/vectors

if [[ -d sdk/node ]]; then
  rm -f sdk/node/mdc-node-native.node
  (cd sdk/node && node --test index.test.js)
  cargo build -p mdc-node-native -p mdc-sim-peer
  if [[ -f target/debug/libmdc_node_native.dylib ]]; then
    cp target/debug/libmdc_node_native.dylib sdk/node/mdc-node-native.node
  elif [[ -f target/debug/libmdc_node_native.so ]]; then
    cp target/debug/libmdc_node_native.so sdk/node/mdc-node-native.node
  fi
  node --test sdk/node/native-smoke.test.js
  rm -f sdk/node/mdc-node-native.node
fi
if [[ -f examples/electron-demo/demo-logic.test.js ]]; then
  node --test examples/electron-demo/demo-logic.test.js
fi
if command -v swiftc >/dev/null 2>&1; then
  cargo build -p mdc_c
  # Rosetta 里的 swiftc 会选 x86_64 切片，链不上 arm64 的 libmdc_c。
  SWIFTC=(swiftc)
  if file target/debug/libmdc_c.dylib | grep -q 'arm64'; then
    SWIFTC=(arch -arm64 swiftc)
  fi
  "${SWIFTC[@]}" -parse-as-library -import-objc-header sdk/c/include/mdc.h \
    -L target/debug -lmdc_c \
    -Xlinker -rpath -Xlinker "$ROOT/target/debug" \
    sdk/swift/Sources/MdcSession.swift sdk/swift/main.swift \
    -o /tmp/mdc-swift-hello
  /tmp/mdc-swift-hello
  cargo build -p mdc-sim-peer
  PEER_PORT="$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)"
  ./target/debug/mdc-sim-peer "127.0.0.1:${PEER_PORT}" >/tmp/mdc-sim-peer-swift.log 2>&1 &
  PEER_PID=$!
  python3 - "$PEER_PORT" <<'PY'
import socket, sys, time
port = int(sys.argv[1])
deadline = time.time() + 2
while time.time() < deadline:
    try:
        sock = socket.create_connection(("127.0.0.1", port), 0.2)
        sock.close()
        break
    except OSError:
        time.sleep(0.05)
else:
    raise SystemExit("sim peer did not listen")
PY
  sleep 0.2
  "${SWIFTC[@]}" -parse-as-library -import-objc-header sdk/c/include/mdc.h \
    -L target/debug -lmdc_c \
    -Xlinker -rpath -Xlinker "$ROOT/target/debug" \
    sdk/swift/Sources/MdcSession.swift sdk/swift/tcp_main.swift \
    -o /tmp/mdc-swift-tcp
  MDC_TCP_ADDR="127.0.0.1:${PEER_PORT}" /tmp/mdc-swift-tcp || {
    kill "$PEER_PID" || true
    exit 1
  }
  kill "$PEER_PID" || true
fi

cargo build -p mdc -p mdc-sim-peer
PAIR_STORE="$(mktemp)"
MDC_PAIRING_STORE="$PAIR_STORE" ./target/debug/mdc sim-accept >/tmp/mdc-sim-accept.json
python3 - <<'PY'
import json
report = json.load(open("/tmp/mdc-sim-accept.json"))
assert report["ok"] is True
assert report["peer"] == "mdc-sim-peer", report
assert report["firmware_after"] == "1.0.0"
assert report["ota_failed"] is True
assert report["image_bytes_included"] is False
print("sim-accept", report["peer"], report["firmware_after"])
PY
rm -f "$PAIR_STORE"

LINUX_PORT="$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)"
./target/debug/mdc-sim-peer "127.0.0.1:${LINUX_PORT}" linux-virt >/tmp/mdc-linux-accept.log 2>&1 &
LINUX_PID=$!
LINUX_STORE="$(mktemp)"
if ! MDC_PAIRING_STORE="$LINUX_STORE" ./target/debug/mdc pair sim-linux-virt token \
  || ! MDC_PAIRING_STORE="$LINUX_STORE" ./target/debug/mdc --address "127.0.0.1:${LINUX_PORT}" linux-accept; then
  kill "$LINUX_PID" || true
  cat /tmp/mdc-linux-accept.log || true
  rm -f "$LINUX_STORE"
  exit 1
fi
kill "$LINUX_PID" || true
rm -f "$LINUX_STORE"

CXX_PORT="$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)"
c++ -std=c++17 -I runtime/linux/include -I runtime/esp32/include \
  runtime/esp32/src/parser.cpp runtime/esp32/src/frame_store.cpp \
  runtime/esp32/src/sim_display.cpp runtime/esp32/src/dispatcher.cpp \
  runtime/esp32/src/cbor_control.cpp \
  runtime/linux/src/linux_runtime.cpp runtime/linux/main/mdc_linux_peer.cpp \
  -o /tmp/mdc_linux_peer
/tmp/mdc_linux_peer "127.0.0.1:${CXX_PORT}" >/tmp/mdc-linux-cxx.log 2>&1 &
CXX_PID=$!
CXX_STORE="$(mktemp)"
if ! MDC_PAIRING_STORE="$CXX_STORE" ./target/debug/mdc pair sim-linux-virt token \
  || ! MDC_PAIRING_STORE="$CXX_STORE" ./target/debug/mdc --address "127.0.0.1:${CXX_PORT}" linux-accept; then
  kill "$CXX_PID" || true
  cat /tmp/mdc-linux-cxx.log || true
  rm -f "$CXX_STORE"
  exit 1
fi
kill "$CXX_PID" || true
rm -f "$CXX_STORE"

if command -v cmake >/dev/null 2>&1; then
  # Host stub only. A configured IDF_PATH would switch this directory into a device project.
  env -u IDF_PATH cmake -S runtime/esp32/idf -B /tmp/mdc-idf-stub
  cmake --build /tmp/mdc-idf-stub
  ctest --test-dir /tmp/mdc-idf-stub --output-on-failure
else
  cc -std=c11 -I runtime/esp32/idf/port \
    runtime/esp32/idf/main/mdc_app_main.c \
    runtime/esp32/idf/tests/panel_bringup_test.c \
    -o /tmp/mdc_panel_bringup_test
  /tmp/mdc_panel_bringup_test
fi

echo "check.sh: all host/sim gates passed"
