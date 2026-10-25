#!/usr/bin/env bash
# SIM-R08: build the C++ Linux peer and run the Rust-host integration tests.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
BIN="$(mktemp -t mdc-cxx-peer)"
trap 'rm -f "$BIN"' EXIT
CXX="${CXX:-c++}"
"$CXX" -std=c++17 -I runtime/linux/include -I runtime/esp32/include \
  runtime/esp32/src/parser.cpp runtime/esp32/src/frame_store.cpp \
  runtime/esp32/src/sim_display.cpp runtime/esp32/src/dispatcher.cpp \
  runtime/esp32/src/cbor_control.cpp \
  runtime/linux/src/linux_runtime.cpp runtime/linux/main/mdc_linux_peer.cpp \
  -o "$BIN"
MDC_CXX_PEER_BIN="$BIN" cargo test -p mdc-compatibility-tests --test cxx_peer "$@"
