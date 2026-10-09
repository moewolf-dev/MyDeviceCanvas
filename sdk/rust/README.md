# Rust SDK (Host Core)

The host-facing Rust API is the workspace crates themselves — there is no separate published package yet.

| Crate | Role |
|---|---|
| `mdc-protocol` | Packet codec, Capabilities, Frame/Tile/Input/Ota payloads |
| `mdc-transport` | `Transport` trait, `MemoryLink`, `TcpTransport` |
| `mdc-core` | `Session`, `DeviceManager`, frame queues |
| `mdc-simulator` | `FakeDevice` peer for sim-verified tests |

## Minimal example (Session + MemoryLink)

```rust
use mdc_core::{ConnectionState, Frame, Session};
use mdc_simulator::{BoardProfile, FakeDevice};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let profile = BoardProfile {
        width: 2,
        height: 2,
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    let mut session = Session::new(host);
    session.connect()?;
    device.poll()?;
    assert_eq!(session.poll()?, ConnectionState::Ready);

    session.send_frame(Frame {
        surface_id: "main".into(),
        width: 2,
        height: 2,
        bytes: vec![0; 8],
    })?;
    device.poll()?;
    session.poll()?;
    Ok(())
}
```

See also `examples/rust-hello` in this repository for a runnable binary.

License: Apache-2.0
