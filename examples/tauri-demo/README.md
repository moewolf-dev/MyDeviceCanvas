# Tauri command layer

`mdc-tauri-demo` is the Rust API a Tauri shell should call. It depends on `mdc-core` and `mdc-simulator` and does not encode protocol packets itself.

```sh
cargo test -p mdc-tauri-demo
```

`switch_sim_endpoint` cancels the previous session channel. Tiles fail until a full frame is displayed. `inspector()` returns counters and the last error string, not pixel buffers.

The WebView package is not vendored here. A Tauri app adds this crate as a dependency and forwards the same methods through commands.

Physical transports stay pending.

License: Apache-2.0
