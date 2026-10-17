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

Metrics / diagnostics series (bounded JSONL, N317):

```sh
cargo run -p mdc-inspector -- --fresh                 # one pretty snapshot
cargo run -p mdc-inspector -- --jsonl 60 --interval 100 --board linux-virt
cargo run -p mdc -- --board linux-virt diag --metric fps --value 29.5 --unit fps
```

Provisioning a compiled package onto a simulator board (preflight → install →
version readback, N339):

```sh
cargo run -p mdc -- --board linux-virt provision ./model.mdcpkg
```

OTA a simulator board end-to-end (N350):

```sh
cargo run -p mdc -- --board esp32-jc3248w535-sim ota 2.1.0
cargo run -p mdc -- --address 127.0.0.1:9876 ota 2.1.0   # over TCP (N354)
```


