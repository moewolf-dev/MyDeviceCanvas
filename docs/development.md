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
cargo run -p mdc-inspector -- --heap --count 8 --size-kb 1024       # peak memory (N509)
cargo run -p mdc-inspector -- --jsonl 20 --heap --size-kb 512       # peak timeline (N519)
cargo run -p mdc -- --board linux-virt record --frames 30 --fps 10 --dir out
cargo run -p mdc -- record --play --dir out                          # verify (N520)
cargo run -p mdc -- record --play --render --dir out --board linux-virt  # replay (N530)
cargo run -p mdc -- record --play --render --dir out --fps 10        # paced replay (N540)
cargo run -p mdc -- --board linux-virt config --defaults             # board defaults (N626/N636)
cargo run -p mdc-inspector -- --check-archive DIR --max-bytes 1000000 # per-file bytes (N639)
# `--check-archive` exits 0 only when the report is ok (N649)
cargo run -p mdc -- --board linux-virt inspect --defaults-only        # defaults, no session (N646)
cargo run -p mdc -- --board linux-virt inspect --defaults-only --key tile_missing_threshold
# `mdc package --report` prints cache_hit_rate and cache_hit_rate_before (N654)
cargo run -p mdc-inspector -- --jsonl 20 --heap --max-peak-bytes 200000000  # alert (N529)
# the alert ends with one JSONL verdict line and exits 2 (N539)
cargo run -p mdc-inspector -- --jsonl 60 --board linux-virt --out run.jsonl # archive (N560)
cargo run -p mdc-inspector -- --jsonl 60 --out run.jsonl --append --max-bytes 1048576  # append + rotate (N569)
cargo run -p mdc-inspector -- --jsonl 60 --out run.jsonl --max-bytes 1048576 --keep 3  # keep .1...3 (N579)
mkdir -p archive && cargo run -p mdc-inspector -- --jsonl 60 --out archive --append --max-bytes 1048576  # DIR/mdc-history.jsonl (N589)
# N599: DIR also gets a manifest.json listing the live file and its rotations
cargo run -p mdc -- package model.gltf --optimize-geometry --report   # cache_mode (N594)
cargo run -p mdc -- verify out.mdcpkg                                 # + cache_misses (N604)
cargo run -p mdc -- --board linux-virt inspect                        # board_config_defaults (N606)
cargo run -p mdc-inspector -- --check-archive archive                 # validate archive (N619)
cargo run -p mdc-inspector -- --check-archive archive --max-bytes 1048576  # cap (N629)
cargo run -p mdc -- validate model.gltf --strict                      # KTX2 notes fail strict (N620)
cargo run -p mdc -- --board linux-virt config --defaults --key tile_missing_threshold  # key parity (N696)
cargo run -p mdc -- --board linux-virt inspect --defaults --key tile_missing_threshold
cargo run -p mdc -- --board linux-virt verify out.mdcpkg --strict  # locality contract (N700)  # one key (N666)
cargo run -p mdc-inspector -- --check-archive archive --compact       # one-line report (N668)
cargo run -p mdc -- package model.gltf --optimize-geometry --report   # index_cache_saved (N669)
cargo run -p mdc -- --board linux-virt inspect                        # config_defaults merged (N676)
cargo run -p mdc -- diff a.mdcpkg b.mdcpkg                            # index_cache_saved (N680)
cargo run -p mdc -- --board linux-virt inspect --defaults              # alias of --defaults-only (N686)
cargo run -p mdc-inspector -- --check-archive archive --json          # pretty alias (N689)
cargo run -p mdc -- verify out.mdcpkg                                 # manifest_cache_misses (N690)
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

`FakeDevice::hit_test(x, y)` (N418) returns the topmost scene node under a point,
and device input events carry that node name (`InputEvent.node`; the Node SDK
exposes it as `input.node`).

`mdc bake --mipmaps` (N396) also writes a box-downsampled mip chain
(`mip0.ppm` … ) beside the atlas, produced by `mdc_bake::bake_mipmaps`.
`mdc bake --dither N` (N404) applies ordered (Bayer) dithering (strength `N` LSBs)
to the atlas and its mip chain via `mdc_bake::bayer_dither_rgb565`.
`mdc bake --palette N` (N405) median-cut quantizes the atlas to at most `N` palette
entries (`mdc_bake::quantize_palette`), writing `atlas.pal.json` + `atlas.idx`.

`Session::stats()` (N398) returns host counters (`packets_sent`, `bytes_sent`,
`acks_received`, `frames_sent`); `mdc inspect --watch` reports them per tick.
`Session::ota_phase()` / `reset_ota()` (N419) checkpoint the OTA phase
(begin/chunk/confirm) so a driver can resume after a reconnect.

`Session::send_input(event)` (N400) sends a pointer event to the device when it
advertises the `touch` capability; the C ABI `mdc_session_send_input(...)` and the
Node `_native.sendInput(...)` wrap it.

N704: `mdc package --optimize-geometry`/`--optimize-index` persists the
before/after vertex-cache summary in the package manifest
(`cache_misses_before`/`cache_misses_saved`); `mdc verify` and `mdc diff` surface
it, and the fields are metadata (the body checksum is unchanged).

N709: `mdc-inspector --prune-archive DIR --keep N` keeps the live
`mdc-history.jsonl` plus `.1..N` rotated copies and deletes higher rotations
(unrelated files are left alone). Combine with `--check-archive DIR` to verify
the resulting manifest. N719: when `manifest.json` exists, pruning also rewrites
its `files`/`rotated`/`keep` and refreshes `generatedAt`, so the manifest never
describes deleted files.

N710: `mdc verify`, `mdc diff` and `mdc model-info` share one KTX2 summary
(`ktx2_textures`, `ktx2_slices`, `ktx2_size_mismatches`); `diff` also emits a
`ktx2` field so a metadata mismatch is visible without inspecting structure
counts. N720: `mdc verify` additionally prints `compatibility_row`, a
ready-to-paste matrix row derived from that same summary.

N728: `Session::ota_resume_commands(version, size, sha256)` filters the OTA
begin/chunk/confirm sequence by `ota_phase`, so a driver that reconnects only
sends what the device has not acknowledged; `mdc ota` uses it for both the
simulator and TCP paths.

N729: `mdc_assets::ChunkStore` stores fixed-size chunks once (SHA-256 keys) and
returns a `StoredBlob` index; `release` reference-counts chunks and the quota is
checked before any mutation.

N732: `DirtyTracker::adapt_to_bandwidth(bytes_per_second, frame_bytes)` maps the
measured link to a full-frame threshold (0.05 below ~1 FPS, up to 0.9 at 8+ FPS),
and `Session::measure_bitrate`/`adapt_dirty_threshold` derive the rate from the
outbound byte counter and the virtual clock.

N725: `GestureKind::Pinch` carries `scale` (permille) and `rotation` (degrees)
from two simultaneous pointers; both fingers are consumed so their release is
not reported as a tap/drag.

N730: `AtlasClip.mode` (`loop`/`once`/`ping-pong`) and `speed_permille` scale
device-side atlas playback; `animation_loops()` accounts for both.

N737: `OtaCommand.offset/len` mark a chunk range; `Session::ota_chunk_ranges` and
`ota_missing_chunks` compute the ranges still needed, and
`ota_resume_commands_chunked` emits begin (only at phase 0), the missing chunks
and confirm. The simulator records/merges received ranges
(`FakeDevice::ota_received_ranges`) and rejects a range outside the image.

N738: `mdc_assets::DiskChunkStore` persists content-addressed chunks and an
`index.json`; reopening restores dedup, `release` reference-counts blobs and
deletes chunks whose last reference is gone.

N739: `mdc package --trim-textures` calls `trim_package_textures`, cropping
transparent texture margins and remapping mesh UVs before atlas/mip work.

N741: `Scheduler::adapt_to_bitrate(rate, frame_bytes)` caps FPS at the
sustainable rate and never budgets above the measured rate;
`Session::adapt_frame_pacing` applies it to the active scheduler.

N743/N744: `path` also accepts an `M/L/Q/C/Z` command stream (64-command bound),
and `mask` clips child nodes to a polygon. `mdc scene-lint` accepts both.

N747: `DiskChunkStore::compact()` reindexes the directory, drops blobs with
missing chunks, deletes orphan/temporary files and returns a
`DiskMaintenance { missing_blobs, orphan_chunks, temp_files, bytes_reclaimed }`.

N750: `Session::tune_streaming(&mut DirtyTracker, frame_bytes)` measures the
rate once and updates both the scheduler and the dirty threshold; it returns
`AdaptiveTuning { bitrate, pacing, dirty_threshold }`.

N752: `mdc texture-report <image>` decodes PNG/JPEG/WebP and reports opaque
coverage, the opaque bounding box and trimmable RGB565 bytes; a fully
transparent image exits 1.


