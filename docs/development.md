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
cargo run -p mdc-inspector -- --rss                     # one host memory sample (N370)
cargo run -p mdc-inspector -- --rss-delta --count 5 --interval 20   # RSS growth (N410)
cargo run -p mdc -- --board linux-virt inspect --watch --count 3 --rss
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

A bounded, content-addressed asset store on disk (N369). Files are named by
their SHA-256 key; `put` prunes to `--max-total` (LRU by modification time):

```sh
cargo run -p mdc -- asset put ./cache ./frame.bin --max-total 8388608
cargo run -p mdc -- asset ls ./cache
cargo run -p mdc -- asset get ./cache <key> --out frame.bin
cargo run -p mdc -- asset prune ./cache --max-total 4194304
```

`mdc model-info <model.json|.gltf|.glb>` (N388) prints a JSON import report
(node/mesh/vertex/material/texture/animation counts plus normals/colours/tangents/
second-UV/second-skin feature flags):

```sh
cargo run -p mdc -- model-info ./model.glb
```

`mdc diff <a.mdcpkg> <b.mdcpkg>` (N389) prints a JSON field-by-field comparison of
two sealed packages (manifest fields plus structural counts, with an `identical`
flag):

```sh
cargo run -p mdc -- diff ./old.mdcpkg ./new.mdcpkg
```

`mdc verify <file.mdcpkg>` (N408) checks a sealed package's manifest against its
body (`MdcPackage::verify`), printing a JSON result and exiting non-zero on a
checksum/size mismatch.

`mdc scene-lint <scene.json>` (N417) lints a scene tree (`SceneNode::lint`):
unknown kinds, missing `rect`/`value`, zero extents, duplicate names, and
node/depth limits, exiting non-zero on a fatal problem.

`mdc bake --mipmaps` (N396) also writes a box-downsampled mip chain
(`mip0.ppm` … ) beside the atlas, produced by `mdc_bake::bake_mipmaps`.
`mdc bake --dither N` (N404) applies ordered (Bayer) dithering (strength `N` LSBs)
to the atlas and its mip chain via `mdc_bake::bayer_dither_rgb565`.
`mdc bake --palette N` (N405) median-cut quantizes the atlas to at most `N` palette
entries (`mdc_bake::quantize_palette`), writing `atlas.pal.json` + `atlas.idx`.

`Session::stats()` (N398) returns host counters (`packets_sent`, `bytes_sent`,
`acks_received`, `frames_sent`); `mdc inspect --watch` reports them per tick.

`Session::send_input(event)` (N400) sends a pointer event to the device when it
advertises the `touch` capability; the C ABI `mdc_session_send_input(...)` and the
Node `_native.sendInput(...)` wrap it.


