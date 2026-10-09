//! Minimal Session + FakeDevice (MemoryLink) hello.
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
    let id = session
        .device
        .as_ref()
        .map(|d| d.capabilities.device_id.clone())
        .unwrap_or_default();
    session.send_frame(Frame {
        surface_id: "main".into(),
        width: 2,
        height: 2,
        bytes: vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88],
    })?;
    device.poll()?;
    session.poll()?;
    println!("hello ok: device={id} frame_id={}", device.current_frame_id());
    Ok(())
}
