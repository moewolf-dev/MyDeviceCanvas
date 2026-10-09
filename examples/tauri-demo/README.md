# Tauri Demo (skeleton)

This example is intentionally minimal: a Tauri app would depend on the **Rust SDK crates** (`mdc-core`, `mdc-transport`, `mdc-simulator`) from the workspace rather than re-implementing the protocol.

## Suggested layout

```text
examples/tauri-demo/
  src-tauri/Cargo.toml   # mdc-core = { path = "../../../crates/mdc-core" }
  src-tauri/src/lib.rs   # commands wrapping Session + FakeDevice for --sim
  src/                   # UI shell
```

## Dependency

```toml
[dependencies]
mdc-core = { path = "../../../crates/mdc-core" }
mdc-simulator = { path = "../../../crates/mdc-simulator" }
```

Use `FakeDevice::pair` for desktop sim development; swap transport for TCP/serial when hardware is available. Physical: pending.

License: Apache-2.0
