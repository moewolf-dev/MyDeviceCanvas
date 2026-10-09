# Node Native Module

This crate is the ownership boundary for the Node SDK. `send_frame` copies the incoming N-API Buffer into Rust-owned memory. `dispose` releases that memory and all later calls return an error. It does not expose Rust internal DTOs or retain JavaScript pointers.

Build from the repository root with:

```sh
cargo build -p mdc-node-native
```

Packaging the platform-specific `.node` artifact is intentionally separate from host Rust tests and must declare the tested Node ABI and macOS architecture.
