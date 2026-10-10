# Development

Requirements: Rust stable and Node.js 20+.

```sh
./tools/check.sh
```

Or run pieces individually:

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo test --workspace --all-features
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p mdc -- devices
cargo run -p mdc -- simulate --board esp32-jc3248w535-sim
```

Host-side tests are deterministic. Simulator verification is the default gate.
Physical-board and ESP-IDF target builds are optional and must be labeled separately.

Portable C++ runtime (macOS host):

```sh
c++ -std=c++17 -I runtime/esp32/include \
  runtime/esp32/src/parser.cpp runtime/esp32/src/frame_store.cpp \
  runtime/esp32/src/sim_display.cpp runtime/esp32/tests/parser_test.cpp \
  -o /tmp/mdc_runtime_tests
/tmp/mdc_runtime_tests
```

Board profiles:

```sh
python3 tools/validate_board.py boards/sim/esp32-jc3248w535-sim/board.yaml
python3 tools/validate_board.py boards/sim/linux-virt/board.yaml
python3 tools/validate_board.py boards/linux/pi4-hdmi/board.yaml
python3 tools/validate_board.py boards/linux/waveshare-eink/board.yaml
node --test examples/electron-demo/demo-logic.test.js
python3 tools/validate_board.py boards/esp32/test-valid/board.yaml
python3 tools/validate_board.py boards/esp32/jc3248w535/board.yaml  # expected failure: unverified unknowns
python3 -m unittest discover -s tools -p 'test_*.py'
```
