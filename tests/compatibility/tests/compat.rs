//! Compatibility gates: major mismatch and low negotiated limits.
use mdc_core::{ConnectionState, CoreError, Frame, Session, MAX_FRAME_BYTES};
use mdc_protocol::{encode_control, Capabilities, MessageType, Packet, Surface, VERSION};
use mdc_simulator::{BoardProfile, FakeDevice};
use mdc_transport::MemoryLink;

#[test]
fn major_version_mismatch_is_rejected_at_decode() {
    let (_host, _peer) = MemoryLink::pair();
    // Craft a packet with wrong major; Packet::decode must fail before Session handles it.
    let mut bytes = Packet {
        version: VERSION,
        kind: MessageType::Capabilities,
        flags: 0,
        request_id: 1,
        payload: encode_control(&Capabilities {
            device_id: "x".into(),
            firmware: "0".into(),
            surfaces: vec![Surface {
                id: "main".into(),
                width: 1,
                height: 1,
                pixel_format: "RGB565".into(),
                stride: 2,
                rotation: 0,
            }],
            frame: true,
            tile: false,
            touch: false,
            ota: false,
            max_message: 1024,
            max_chunk: 256,
            max_in_flight: 1,
            max_fps: 10,
            mirror: mdc_protocol::MirrorCapability::disabled(),
            local_display_fps: 0,
            mirror_fps: 0,
            mirror_bandwidth: 0,
            renderer: None,
        })
        .unwrap(),
    }
    .encode(4096)
    .unwrap();
    bytes[2] = 99; // corrupt major
    assert!(matches!(
        Packet::decode(&bytes, 4096),
        Err(mdc_protocol::CodecError::Major(99))
    ));
    let _ = (_host, _peer);
}

#[test]
fn fake_device_rejects_hello_with_foreign_major() {
    let profile = BoardProfile {
        width: 2,
        height: 2,
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    let mut session = Session::new(host);
    // Bypass Session::connect and write a forged HELLO with major=2 via transport.
    use mdc_protocol::Hello;
    use mdc_transport::Transport;
    let hello = encode_control(&Hello {
        major: 2,
        minor: 0,
        max_message: 4096,
        max_chunk: 1024,
    })
    .unwrap();
    let pkt = Packet {
        version: VERSION, // header major still 1 so it decodes; payload major is what FakeDevice checks
        kind: MessageType::Hello,
        flags: 0,
        request_id: 1,
        payload: hello,
    };
    session
        .transport_mut()
        .write(&pkt.encode(4096).unwrap())
        .unwrap();
    // FakeDevice replies with Error for unsupported major in Hello payload.
    device.poll().unwrap();
    // Host session was not in handshaking; just ensure device recorded an error metric.
    assert!(device.metrics.errors_sent >= 1 || device.metrics_snapshot().errors_sent >= 1);
}

#[test]
fn low_max_message_and_chunk_are_honored() {
    let profile = BoardProfile {
        width: 4,
        height: 2,
        device_id: "low-limits".into(),
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    device.max_chunk = 8;
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    let caps = &session.device.as_ref().unwrap().capabilities;
    assert!(caps.max_chunk <= 8 || caps.max_chunk == device.max_chunk as u32);
    // Frame larger than max_chunk must chunk successfully under low limits.
    let pixels: Vec<u8> = (0..16).collect();
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 4,
            height: 2,
            bytes: pixels.clone(),
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    device.assert_pixels_eq(&pixels);
}

#[test]
fn oversized_frame_rejected_by_core() {
    let profile = BoardProfile {
        width: 2,
        height: 2,
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    let too_big = vec![0u8; MAX_FRAME_BYTES + 1];
    // Dimension mismatch / size: surface expects 8 bytes
    assert_eq!(
        session.send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: too_big,
        }),
        Err(CoreError::TooLarge)
    );
}

// ---- N37: runtime package + mirror fixtures ----

fn sample_package() -> mdc_model::MdcPackage {
    let gltf = br#"{"asset":{"version":"2.0"},"nodes":[{"name":"root","mesh":0}],
        "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
        "accessors":[{"count":4,"type":"VEC3"}]}"#;
    let model = mdc_model::GltfImporter
        .import(gltf, &mdc_model::ImportLimits::default())
        .unwrap();
    mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::sprite_only()).unwrap()
}

#[test]
fn package_rejects_bad_magic_and_oversized_headers() {
    let package = sample_package();
    let mut bytes = package.to_bytes().unwrap();
    bytes[0] = b'X';
    assert!(matches!(
        mdc_model::MdcPackage::from_bytes(&bytes),
        Err(mdc_model::ModelError::Unsupported(_))
    ));

    let mut header = vec![0u8; 10];
    header[0..4].copy_from_slice(b"MDCP");
    header[5] = 1;
    header[6..10].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(matches!(
        mdc_model::MdcPackage::from_bytes(&header),
        Err(mdc_model::ModelError::Budget(_))
    ));
}

#[test]
fn package_capability_gate_follows_the_device_renderer() {
    let gltf = br#"{"asset":{"version":"2.0"},"nodes":[{"name":"root","mesh":0}],
        "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
        "accessors":[{"count":4,"type":"VEC3"}]}"#;
    let model = mdc_model::GltfImporter
        .import(gltf, &mdc_model::ImportLimits::default())
        .unwrap();
    let mesh = mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::full_3d())
        .unwrap();
    assert!(mesh.accepted_by(&mdc_model::RendererCapability::full_3d()).is_ok());
    assert!(matches!(
        mesh.accepted_by(&mdc_model::RendererCapability::sprite_only()),
        Err(mdc_model::ModelError::Unsupported(_))
    ));
}

#[test]
fn full_frame_receiver_refuses_a_dirty_region_commit() {
    use mdc_mirror::{MirrorReceiver, MockObsSink};
    let mut receiver =
        MirrorReceiver::new("main", 2, 1, "full-frame", MockObsSink::default()).unwrap();
    let packet = |payload: Vec<u8>| {
        Packet {
            version: VERSION,
            kind: MessageType::MirrorFrame,
            flags: 0,
            request_id: 0,
            payload,
        }
        .encode(1024 * 1024)
        .unwrap()
    };
    receiver
        .feed(&packet(
            mdc_protocol::encode_frame_payload("main", 1, 2, 1, &[0x00, 0xF8, 0xE0, 0x07]).unwrap(),
        ))
        .unwrap();
    // A dirty patch (only the second pixel) must be refused by a full-frame sink.
    receiver
        .feed(&packet(
            mdc_protocol::encode_frame_begin("main", 2, 2, 1, 4).unwrap(),
        ))
        .unwrap();
    receiver
        .feed(&packet(mdc_protocol::encode_frame_chunk(2, 2, &[0x1F, 0x00])))
        .unwrap();
    receiver
        .feed(&packet(mdc_protocol::encode_frame_commit(2)))
        .unwrap();
    let stats = receiver.stats();
    assert_eq!(stats.frames_published, 1);
    assert!(stats.frames_dropped >= 1);
}
