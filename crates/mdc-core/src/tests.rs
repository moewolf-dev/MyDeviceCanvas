use super::*;
use mdc_protocol::{MessageType, Packet};
use mdc_transport::MemoryLink;
fn caps() -> Capabilities {
    Capabilities {
        device_id: "hb".into(),
        board_id: String::new(),
        protocol_version: 0,
        firmware: "1".into(),
        surfaces: vec![s()],
        frame: true,
        tile: true,
        touch: false,
        ota: false,
        max_message: 4096,
        max_chunk: 16384,
        max_in_flight: 1,
        max_fps: 30,
        mirror: mdc_protocol::MirrorCapability::disabled(),
        local_display_fps: 0,
        mirror_fps: 0,
        mirror_bandwidth: 0,
        renderer: None,
        config: mdc_protocol::ConfigCapability::default(),
        scene: mdc_protocol::SceneCapability::default(),
        boot_id: 0,
        run_id: 0,
    }
}
fn s() -> Surface {
    Surface {
        id: "main".into(),
        width: 2,
        height: 2,
        pixel_format: "RGB565".into(),
        stride: 4,
        rotation: 0,
        preferred_tile_size: 0,
        supports_partial_update: false,
        supports_full_framebuffer: false,
    }
}

#[test]
fn surface_weight_config_parser_is_bounded() {
    // N777: 1..=4 entries, each 1..=16; whitespace tolerated.
    assert_eq!(parse_surface_weights("3,1"), Some(vec![3, 1]));
    assert_eq!(parse_surface_weights(" 2 , 4 "), Some(vec![2, 4]));
    for bad in ["", "0", "17", "1,0", "1,2,3,4,5", "1,x", "-1"] {
        assert_eq!(parse_surface_weights(bad), None, "{bad}");
    }
}

#[test]
fn animation_blend_config_parser_is_bounded() {
    // N833: `off`/empty clear the blend; entries are name:weight with unique
    // names, weights 1..=1000, and at most four clips.
    assert_eq!(parse_animation_blend("off"), Some(Vec::new()));
    assert_eq!(parse_animation_blend("  "), Some(Vec::new()));
    assert_eq!(
        parse_animation_blend("idle:700, wave:300"),
        Some(vec![("idle".into(), 700), ("wave".into(), 300)])
    );
    assert_eq!(
        format_animation_blend(&[("idle".into(), 700), ("wave".into(), 300)]),
        "idle:700,wave:300"
    );
    assert_eq!(format_animation_blend(&[]), "off");
    for bad in [
        "idle",
        "idle:0",
        "idle:1001",
        "idle:-1",
        "idle:1,idle:2",
        ":5",
        "off:5",
        "a:1,b:2,c:3,d:4,e:5",
        "this-name-is-longer-than-thirty-two-bytes:1",
    ] {
        assert_eq!(parse_animation_blend(bad), None, "{bad}");
    }
}

#[test]
fn shared_surface_plan_fairly_advances_two_displays() {
    // N758: one KiB of budget selects one tile overall; both surfaces are
    // represented, so 2 KiB serves the first tile of each.
    let mut capabilities = caps();
    capabilities.surfaces = vec![
        Surface {
            id: "main".into(),
            width: 64,
            height: 64,
            stride: 128,
            ..s()
        },
        Surface {
            id: "aux".into(),
            width: 64,
            height: 64,
            stride: 128,
            ..s()
        },
    ];
    let device = Device::new(capabilities);
    let tight = device.plan_shared_tiles(1024);
    assert_eq!(tight.surfaces.len(), 2);
    assert_eq!(tight.surfaces[0].selected, vec![0]);
    assert!(tight.surfaces[1].selected.is_empty());
    assert_eq!(tight.surfaces[0].deferred.len(), 3, "64px/32px = 4 tiles");
    let roomy = device.plan_shared_tiles(2048);
    assert_eq!(roomy.surfaces[0].selected, vec![0]);
    assert_eq!(roomy.surfaces[1].selected, vec![0]);
    // N767: weights share only the extra budget; both surfaces move first.
    let weighted = device.plan_shared_tiles_weighted(3072, &[2, 1]);
    assert_eq!(weighted.surfaces[0].selected, vec![0, 1]);
    assert_eq!(weighted.surfaces[1].selected, vec![0]);
    assert_eq!(weighted.spent, 3072);
}

#[test]
fn validates_frame() {
    let mut q = SurfaceQueue::new(s());
    assert!(q
        .enqueue_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![0; 8]
        })
        .is_ok());
    assert_eq!(
        q.enqueue_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![0; 7]
        }),
        Err(CoreError::TooLarge)
    );
}

#[test]
fn surface_geometry_writeback_updates_caps_and_queue() {
    let caps = Capabilities {
        device_id: "x".into(),
        board_id: String::new(),
        protocol_version: 0,
        firmware: "0".into(),
        surfaces: vec![s()],
        frame: true,
        tile: true,
        touch: false,
        ota: false,
        max_message: 4096,
        max_chunk: 16384,
        max_in_flight: 1,
        max_fps: 30,
        mirror: mdc_protocol::MirrorCapability::disabled(),
        local_display_fps: 0,
        mirror_fps: 0,
        mirror_bandwidth: 0,
        renderer: None,
        config: mdc_protocol::ConfigCapability::default(),
        scene: mdc_protocol::SceneCapability::default(),
        boot_id: 0,
        run_id: 0,
    };
    let mut d = Device::new(caps);
    assert!(!d.update_surface_geometry("main", 2, 2).unwrap());
    assert!(d.update_surface_geometry("main", 480, 320).unwrap());
    assert_eq!(d.capabilities.surfaces[0].width, 480);
    assert_eq!(d.capabilities.surfaces[0].stride, 960);
    assert_eq!(d.surfaces[0].info.height, 320);
}
#[test]
fn tile_bounds_checked() {
    let c = Capabilities {
        device_id: "x".into(),
        board_id: String::new(),
        protocol_version: 0,
        firmware: "0".into(),
        surfaces: vec![s()],
        frame: true,
        tile: true,
        touch: false,
        ota: false,
        max_message: 4096,
        max_chunk: 16384,
        max_in_flight: 1,
        max_fps: 30,
        mirror: mdc_protocol::MirrorCapability::disabled(),
        local_display_fps: 0,
        mirror_fps: 0,
        mirror_bandwidth: 0,
        renderer: None,
        config: mdc_protocol::ConfigCapability::default(),
        scene: mdc_protocol::SceneCapability::default(),
        boot_id: 0,
        run_id: 0,
    };
    let d = Device::new(c);
    assert_eq!(
        d.send_tile(&Tile {
            surface_id: "main".into(),
            base_frame_id: 1,
            x: 2,
            y: 0,
            width: 1,
            height: 1,
            bytes: vec![0; 2]
        }),
        Err(CoreError::TileBounds)
    );
    assert_eq!(d.touch(), Err(CoreError::Unsupported));
}

#[test]
fn reconnect_policy_is_bounded_and_explicit_disconnect_wins() {
    let policy = ReconnectPolicy {
        max_attempts: 3,
        base_delay_ms: 1_000,
        max_delay_ms: 30_000,
        jitter_ms: 10,
    };
    assert_eq!(policy.delay_ms(1, 4), Some(1_004));
    assert_eq!(policy.delay_ms(1, 40), Some(1_010));
    assert_eq!(policy.delay_ms(3, 20), Some(4_010));
    assert_eq!(policy.delay_ms(4, 0), None);
    let tight = ReconnectPolicy {
        max_attempts: 3,
        base_delay_ms: 1_000,
        max_delay_ms: 1_005,
        jitter_ms: 10,
    };
    assert_eq!(tight.delay_ms(1, 0), Some(1_000));
    assert_eq!(tight.delay_ms(1, 9), Some(1_005));
    let (link, _) = MemoryLink::pair();
    let mut session = Session::new(link);
    assert_eq!(session.mark_transport_failure(), Some(1_000));
    session.disconnect();
    assert_eq!(session.mark_transport_failure(), None);
    assert_eq!(session.state, ConnectionState::Disconnected);
}
#[test]
fn touch_rotation_is_bounded() {
    assert_eq!(transform_touch(480, 320, 90, 0, 0), Ok((319, 0)));
    assert_eq!(transform_touch(480, 320, 180, 479, 319), Ok((0, 0)));
    assert_eq!(
        transform_touch(480, 320, 0, 480, 0),
        Err(CoreError::TileBounds)
    );
}

fn handshake<T: mdc_transport::Transport>(session: &mut Session<T>, device: &mut MemoryLink) {
    let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: hello.request_id,
                payload: encode_control(&caps()).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
}

#[test]
fn large_package_upload_stays_in_the_control_window() {
    // SIM-R03/SIM-A04: 600 KiB would exceed the 32-entry control window if
    // BEGIN/chunks were tracked individually.
    struct Sink {
        writes: usize,
    }
    impl mdc_transport::Transport for Sink {
        fn write(&mut self, bytes: &[u8]) -> Result<(), mdc_transport::TransportError> {
            self.writes += 1;
            assert!(!bytes.is_empty());
            Ok(())
        }
        fn read(&mut self) -> Result<Option<Vec<u8>>, mdc_transport::TransportError> {
            Ok(None)
        }
        fn close(&mut self) {}
    }
    let mut capability = caps();
    capability.renderer = Some(mdc_protocol::RendererCapability::sprite_only(512));
    let mut session = Session::new(Sink { writes: 0 });
    session.state = ConnectionState::Ready;
    session.device = Some(Device::new(capability));
    let bytes = vec![0x5a; 600 * 1024];
    session.install_package(&bytes).unwrap();
    assert_eq!(session.pending_control.len(), 1);
    assert!(session.transport_mut().writes > 1);
}

struct FailAtLink {
    writes: usize,
    fail_at: usize,
}
impl mdc_transport::Transport for FailAtLink {
    fn write(&mut self, _bytes: &[u8]) -> Result<(), mdc_transport::TransportError> {
        self.writes += 1;
        if self.writes == self.fail_at {
            return Err(mdc_transport::TransportError::Io("link lost".into()));
        }
        Ok(())
    }
    fn read(&mut self) -> Result<Option<Vec<u8>>, mdc_transport::TransportError> {
        Ok(None)
    }
    fn close(&mut self) {}
}

fn ready_package_session<T: mdc_transport::Transport>(transport: T) -> Session<T> {
    let mut capability = caps();
    capability.renderer = Some(mdc_protocol::RendererCapability::sprite_only(512));
    let mut session = Session::with_config(
        transport,
        SessionConfig {
            connect_timeout_ms: 100,
            ack_timeout_ms: 100,
            heartbeat_ms: 0,
            reconnect: true,
        },
    );
    session.state = ConnectionState::Ready;
    session.device = Some(Device::new(capability));
    session
}

#[test]
fn package_write_failure_abandons_the_link() {
    // SIM-R03: an interrupted package cannot be resumed chunk-by-chunk.
    let mut session = ready_package_session(FailAtLink {
        writes: 0,
        fail_at: 2,
    });
    let error = session.install_package(&[0x33; 64]).unwrap_err();
    assert!(matches!(error, CoreError::Transport(_)), "{error:?}");
    assert_eq!(session.packages_sent(), 0);
    assert_eq!(session.pending_request_count(), 0);
    assert!(session.pending_control.is_empty());
    assert_eq!(session.state, ConnectionState::Reconnecting);
}

#[test]
fn package_short_write_abandons_the_link() {
    let mut session = ready_package_session(ShortAtLink::new(2));
    assert_eq!(
        session.install_package(&[0x44; 64]),
        Err(CoreError::ShortWrite)
    );
    assert!(session.unsent.is_none());
    assert_eq!(session.packages_sent(), 0);
    assert_eq!(session.pending_request_count(), 0);
    assert_eq!(session.state, ConnectionState::Reconnecting);
}

#[test]
fn full_control_window_rejects_a_package_before_writing() {
    let mut session = ready_package_session(ShortAtLink::new(usize::MAX));
    for id in 0..MAX_CONTROL_IN_FLIGHT as u32 {
        session.pending_control.insert(id);
    }
    assert_eq!(
        session.install_package(&[1, 2, 3]),
        Err(CoreError::ControlQueueFull)
    );
    assert_eq!(session.transport.writes, 0);
}

#[test]
fn install_package_requires_a_renderer_capability() {
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    handshake(&mut session, &mut device);
    assert!(matches!(
        session.install_package(&[1, 2, 3]),
        Err(CoreError::Unsupported)
    ));
    assert_eq!(session.packages_sent(), 0);
}

#[test]
fn install_package_chunks_over_the_link_and_reassembles() {
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    let mut capability = caps();
    capability.renderer = Some(mdc_protocol::RendererCapability::sprite_only(512));
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: hello.request_id,
                payload: encode_control(&capability).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    assert_eq!(session.state, ConnectionState::Ready);

    let gltf = br#"{"asset":{"version":"2.0"},"nodes":[{"name":"root","mesh":0}],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
            "accessors":[{"count":4,"type":"VEC3"}]}"#;
    let model = mdc_model::GltfImporter
        .import(gltf, &mdc_model::ImportLimits::default())
        .unwrap();
    let package =
        mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::sprite_only())
            .unwrap();
    let bytes = package.to_bytes().unwrap();

    session.install_package(&bytes).unwrap();
    assert_eq!(session.packages_sent(), 1);

    // The device side reassembles the container from the PACKAGE stream.
    let mut assembler = mdc_model::PackageAssembler::begin();
    while let Some(raw) = device.read().unwrap() {
        let packet = Packet::decode(&raw, 1 << 20).unwrap().0;
        if packet.kind != MessageType::Package {
            continue;
        }
        match mdc_protocol::decode_package_payload(&packet.payload).unwrap() {
            mdc_protocol::PackagePayload::Begin { .. } => {}
            mdc_protocol::PackagePayload::Chunk { data, .. } => {
                assembler.push_chunk(&data).unwrap()
            }
            mdc_protocol::PackagePayload::Commit => break,
        }
    }
    assert_eq!(assembler.finish().unwrap(), package);
}

#[test]
fn config_ops_are_capability_gated_and_cached() {
    let (host, mut dev) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    let hello = Packet::decode(&dev.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    let mut capability = caps();
    capability.config = mdc_protocol::ConfigCapability::new(&["brightness"]);
    write_caps(&mut dev, hello.request_id, &capability);
    session.poll().unwrap();
    assert_eq!(session.state, ConnectionState::Ready);

    // An unadvertised key is refused without sending anything.
    assert!(matches!(
        session.set_config("wifi_password", "hunter2"),
        Err(CoreError::Unsupported)
    ));

    session.set_config("brightness", "128").unwrap();
    let packet = Packet::decode(&dev.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    assert_eq!(packet.kind, MessageType::Config);
    let command: ConfigCommand = mdc_protocol::decode_control(&packet.payload).unwrap();
    assert_eq!(command.action, "set");
    assert_eq!(command.key, "brightness");
    assert_eq!(command.value.as_deref(), Some("128"));

    let big = "x".repeat(MAX_CONFIG_VALUE_BYTES + 1);
    assert!(matches!(
        session.set_config("brightness", &big),
        Err(CoreError::TooLarge)
    ));

    let reply = ConfigReply {
        key: "brightness".into(),
        ok: true,
        value: Some("200".into()),
        error: None,
    };
    dev.write(
        &Packet {
            version: VERSION,
            kind: MessageType::Config,
            flags: 0,
            request_id: 0,
            payload: encode_control(&reply).unwrap(),
        }
        .encode(4096)
        .unwrap(),
    )
    .unwrap();
    session.poll().unwrap();
    assert_eq!(session.config("brightness"), Some("200"));
}

#[test]
fn animation_helpers_drive_the_device_config() {
    let (host, mut dev) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    let hello = Packet::decode(&dev.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    let mut capability = caps();
    capability.config = mdc_protocol::ConfigCapability::new(&["animation"]);
    write_caps(&mut dev, hello.request_id, &capability);
    session.poll().unwrap();

    session.set_animation("idle").unwrap();
    dev.write(
        &Packet {
            version: VERSION,
            kind: MessageType::Config,
            flags: 0,
            request_id: 0,
            payload: encode_control(&ConfigReply {
                key: "animation".into(),
                ok: true,
                value: Some("idle".into()),
                error: None,
            })
            .unwrap(),
        }
        .encode(4096)
        .unwrap(),
    )
    .unwrap();
    session.poll().unwrap();
    assert_eq!(session.active_animation(), Some("idle"));

    session.stop_animation().unwrap();
    dev.write(
        &Packet {
            version: VERSION,
            kind: MessageType::Config,
            flags: 0,
            request_id: 0,
            payload: encode_control(&ConfigReply {
                key: "animation".into(),
                ok: true,
                value: Some("off".into()),
                error: None,
            })
            .unwrap(),
        }
        .encode(4096)
        .unwrap(),
    )
    .unwrap();
    session.poll().unwrap();
    assert_eq!(session.active_animation(), None);
}

struct ShortAtLink {
    bytes: Vec<u8>,
    writes: usize,
    short_at: usize,
}
impl ShortAtLink {
    fn new(short_at: usize) -> Self {
        Self {
            bytes: Vec::new(),
            writes: 0,
            short_at,
        }
    }
}
impl mdc_transport::Transport for ShortAtLink {
    fn write(&mut self, bytes: &[u8]) -> Result<(), mdc_transport::TransportError> {
        self.writes += 1;
        if self.writes == self.short_at {
            let accepted = (bytes.len() / 2).max(1);
            self.bytes.extend_from_slice(&bytes[..accepted]);
            return Err(mdc_transport::TransportError::ShortWrite { accepted });
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
    fn read(&mut self) -> Result<Option<Vec<u8>>, mdc_transport::TransportError> {
        Ok(None)
    }
    fn close(&mut self) {}
}

fn frame(bytes: &[u8]) -> Frame {
    Frame {
        surface_id: "main".into(),
        width: 2,
        height: 2,
        bytes: bytes.to_vec(),
    }
}

#[test]
fn per_request_deadline_is_not_extended_by_a_new_request() {
    // SIM-R04/SIM-T06.
    let (host_link, mut device_link) = MemoryLink::pair();
    let mut session = Session::with_config(
        host_link,
        SessionConfig {
            connect_timeout_ms: 100,
            ack_timeout_ms: 100,
            heartbeat_ms: 0,
            reconnect: true,
        },
    );
    session.connect().unwrap();
    handshake(&mut session, &mut device_link);
    session.tick(0).unwrap();
    session.send_frame(frame(&[1; 8])).unwrap();
    session.tick(90).unwrap();
    session
        .write_packet(
            MessageType::Config,
            encode_control(&mdc_protocol::ConfigCommand {
                action: "get".into(),
                key: "brightness".into(),
                value: None,
            })
            .unwrap(),
        )
        .unwrap();
    assert_eq!(session.tick(100), Err(CoreError::AckTimeout));
}

#[test]
fn unsolicited_capabilities_do_not_make_a_session_ready() {
    // SIM-R04/SIM-T12.
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    let caps = caps();
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: 999,
                payload: encode_control(&caps).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    assert!(session.poll().is_err());
    assert_ne!(session.state, ConnectionState::Ready);
}

#[test]
fn transport_close_converges_away_from_ready() {
    // SIM-R04/SIM-T10.
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    handshake(&mut session, &mut device);
    assert_eq!(session.state, ConnectionState::Ready);
    device.close();
    assert!(session.poll().is_err());
    assert_ne!(session.state, ConnectionState::Ready);
}

#[test]
fn stale_ack_keeps_the_new_frame_in_flight() {
    let (host_a, mut dev_a) = MemoryLink::pair();
    let mut session = Session::new(host_a);
    session.connect().unwrap();
    handshake(&mut session, &mut dev_a);
    let old_id = session.send_frame(frame(&[1; 8])).unwrap();
    let _ = dev_a.read().unwrap();
    let (host_b, mut dev_b) = MemoryLink::pair();
    session.switch_transport(host_b).unwrap();
    handshake(&mut session, &mut dev_b);
    let new_id = session.send_frame(frame(&[2; 8])).unwrap();
    assert_ne!(old_id, new_id);
    assert_eq!(session.send_frame(frame(&[3; 8])).unwrap(), 0);
    dev_b
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Ack,
                flags: 0,
                request_id: old_id,
                payload: encode_control(&Ack {
                    received: true,
                    displayed: true,
                    frame_id: 1,
                    error: None,
                    run_id: 0,
                    event_seq: 0,
                })
                .unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    assert!(session.needs_full_frame);
    assert_eq!(
        session.device.as_ref().unwrap().surfaces[0].in_flight(),
        Some(1)
    );
    let mut got = Vec::new();
    while let Some(chunk) = dev_b.read().unwrap() {
        got.extend(chunk);
    }
    assert!(got.windows(8).any(|window| window == [2; 8]));
    assert!(!got.windows(8).any(|window| window == [3; 8]));
}

#[test]
fn short_write_resumes_one_frame_and_blocks_the_next() {
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(mdc_transport::ShortWriteLink::new(host));
    session.connect().unwrap();
    handshake(&mut session, &mut device);
    session.transport_mut().arm(8);
    assert_eq!(
        session.send_frame(frame(&[2; 8])),
        Err(CoreError::ShortWrite)
    );
    let inflight = session.device.as_ref().unwrap().surfaces[0].in_flight();
    assert!(inflight.is_some());
    assert!(session.device.as_ref().unwrap().surfaces[0]
        .pending_frame_id()
        .is_none());
    assert_eq!(
        session.send_frame(frame(&[3; 8])),
        Err(CoreError::ShortWrite)
    );
    assert_eq!(
        session.device.as_ref().unwrap().surfaces[0].in_flight(),
        inflight
    );
    assert!(session.device.as_ref().unwrap().surfaces[0]
        .pending_frame_id()
        .is_none());
    session.resume_write().unwrap();
    let mut got = Vec::new();
    while let Some(chunk) = device.read().unwrap() {
        got.extend(chunk);
    }
    let (packet, consumed) = Packet::decode(&got, 4096).unwrap();
    assert_eq!(consumed, got.len());
    assert_eq!(packet.kind, MessageType::Frame);
    assert!(packet.payload.windows(8).any(|window| window == [2; 8]));
    assert!(!packet.payload.windows(8).any(|window| window == [3; 8]));
}

#[test]
fn chunked_frame_resumes_begin_chunks_and_commit() {
    // SIM-R03: a short BEGIN must resume the whole transaction, not just the
    // current packet.
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(mdc_transport::ShortWriteLink::new(host));
    session.connect().unwrap();
    let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    let mut capabilities = caps();
    capabilities.max_chunk = 4;
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: hello.request_id,
                payload: encode_control(&capabilities).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    session.transport_mut().arm(5);
    assert_eq!(
        session.send_frame(frame(&[7; 8])),
        Err(CoreError::ShortWrite)
    );
    session.resume_write().unwrap();
    let mut bytes = Vec::new();
    while let Some(chunk) = device.read().unwrap() {
        bytes.extend(chunk);
    }
    let mut phases = Vec::new();
    let mut offsets = Vec::new();
    let mut remaining = &bytes[..];
    while !remaining.is_empty() {
        let (packet, consumed) = Packet::decode(remaining, 4096).unwrap();
        assert_eq!(packet.kind, MessageType::Frame);
        phases.push(packet.payload[0]);
        if packet.payload[0] == 2 {
            offsets.push(u32::from_le_bytes([
                packet.payload[9],
                packet.payload[10],
                packet.payload[11],
                packet.payload[12],
            ]));
        }
        remaining = &remaining[consumed..];
    }
    assert_eq!(phases, vec![1, 2, 2, 3]);
    assert_eq!(offsets, vec![0, 4]);
    assert_eq!(session.pending_request_count(), 1);
}

fn ready_frame_session(short_at: usize) -> Session<ShortAtLink> {
    let mut capability = caps();
    capability.max_chunk = 4;
    let mut session = Session::new(ShortAtLink::new(short_at));
    session.state = ConnectionState::Ready;
    session.max_chunk = 4;
    session.device = Some(Device::new(capability));
    session
}

fn frame_packets(session: &Session<ShortAtLink>) -> (Vec<u8>, Vec<u32>) {
    let mut phases = Vec::new();
    let mut offsets = Vec::new();
    let mut remaining = &session.transport.bytes[..];
    while !remaining.is_empty() {
        let (packet, consumed) = Packet::decode(remaining, 4096).unwrap();
        assert_eq!(packet.kind, MessageType::Frame);
        phases.push(packet.payload[0]);
        if packet.payload[0] == 2 {
            offsets.push(u32::from_le_bytes([
                packet.payload[9],
                packet.payload[10],
                packet.payload[11],
                packet.payload[12],
            ]));
        }
        remaining = &remaining[consumed..];
    }
    (phases, offsets)
}

#[test]
fn short_chunk_resumes_the_remaining_chunks_and_commit() {
    let mut session = ready_frame_session(2);
    assert_eq!(
        session.send_frame(frame(&[7; 8])),
        Err(CoreError::ShortWrite)
    );
    session.resume_write().unwrap();
    let (phases, offsets) = frame_packets(&session);
    assert_eq!(phases, vec![1, 2, 2, 3]);
    assert_eq!(offsets, vec![0, 4]);
    assert_eq!(session.pending_request_count(), 1);
}

#[test]
fn short_commit_resumes_without_resending_the_frame() {
    let mut session = ready_frame_session(4);
    assert_eq!(
        session.send_frame(frame(&[7; 8])),
        Err(CoreError::ShortWrite)
    );
    session.resume_write().unwrap();
    let (phases, offsets) = frame_packets(&session);
    assert_eq!(phases, vec![1, 2, 2, 3]);
    assert_eq!(offsets, vec![0, 4]);
    assert_eq!(session.pending_request_count(), 1);
}

#[test]
fn switch_transport_drops_stale_request_deadlines() {
    // SIM-R06: a new endpoint must not inherit the old link's deadlines.
    let (host_a, mut dev_a) = MemoryLink::pair();
    let mut session = Session::with_config(
        host_a,
        SessionConfig {
            connect_timeout_ms: 100,
            ack_timeout_ms: 50,
            heartbeat_ms: 0,
            reconnect: true,
        },
    );
    session.connect().unwrap();
    handshake(&mut session, &mut dev_a);
    session
        .write_packet(
            MessageType::Config,
            encode_control(&mdc_protocol::ConfigCommand {
                action: "get".into(),
                key: "brightness".into(),
                value: None,
            })
            .unwrap(),
        )
        .unwrap();
    assert_eq!(session.pending_request_count(), 1);
    assert_eq!(session.request_deadlines.len(), 1);
    let (host_b, mut dev_b) = MemoryLink::pair();
    session.switch_transport(host_b).unwrap();
    handshake(&mut session, &mut dev_b);
    assert_eq!(session.pending_request_count(), 0);
    assert!(session.request_deadlines.is_empty());
    // The old deadline would have fired at t=50; the new link is idle.
    session.tick(10_000).unwrap();
}

#[test]
fn disconnect_clears_a_short_written_frame_transaction() {
    let mut session = ready_frame_session(1);
    assert_eq!(
        session.send_frame(frame(&[7; 8])),
        Err(CoreError::ShortWrite)
    );
    assert!(session.frame_tx.is_some());
    assert!(session.unsent.is_some());
    assert_eq!(session.pending_request_count(), 1);
    let written = session.transport.bytes.len();
    session.disconnect();
    assert!(session.frame_tx.is_none());
    assert!(session.unsent.is_none());
    assert!(session.request_deadlines.is_empty());
    assert_eq!(session.pending_request_count(), 0);
    session.resume_write().unwrap();
    assert_eq!(session.transport.bytes.len(), written);
}

#[test]
fn unsolicited_pong_does_not_count_as_liveness() {
    // SIM-R04: only the Pong for the outstanding Ping may clear a miss.
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::with_config(
        host,
        SessionConfig {
            connect_timeout_ms: 0,
            ack_timeout_ms: 0,
            heartbeat_ms: 10,
            reconnect: true,
        },
    );
    session.connect().unwrap();
    handshake(&mut session, &mut device);
    session.tick(10).unwrap();
    let ping = Packet::decode(&device.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    assert_eq!(ping.kind, MessageType::Ping);
    for tick in [20, 30] {
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Pong,
                    flags: 0,
                    request_id: ping.request_id + 1_000,
                    payload: Vec::new(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        assert_eq!(session.tick(tick).unwrap(), ConnectionState::Ready);
        let _ = device.read().unwrap();
    }
    assert_eq!(session.missed_heartbeats, 2);
    assert_eq!(session.tick(40), Err(CoreError::HeartbeatMissed));
}

#[test]
fn matching_pong_clears_the_miss_count() {
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::with_config(
        host,
        SessionConfig {
            connect_timeout_ms: 0,
            ack_timeout_ms: 0,
            heartbeat_ms: 10,
            reconnect: true,
        },
    );
    session.connect().unwrap();
    handshake(&mut session, &mut device);
    session.tick(10).unwrap();
    let ping = Packet::decode(&device.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Pong,
                flags: 0,
                request_id: ping.request_id,
                payload: Vec::new(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    assert!(!session.heartbeat_outstanding);
    assert_eq!(session.missed_heartbeats, 0);
    assert_eq!(session.tick(20).unwrap(), ConnectionState::Ready);
    assert_eq!(session.missed_heartbeats, 0);
}

#[test]
fn unsolicited_config_reply_is_ignored() {
    // SIM-R04: a Config packet without a matching request must not fill the
    // cache or hold a control-window slot.
    let (host, mut dev) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    let hello = Packet::decode(&dev.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    let mut capability = caps();
    capability.config = mdc_protocol::ConfigCapability::new(&["brightness"]);
    dev.write(
        &Packet {
            version: VERSION,
            kind: MessageType::Capabilities,
            flags: 0,
            request_id: hello.request_id,
            payload: encode_control(&capability).unwrap(),
        }
        .encode(4096)
        .unwrap(),
    )
    .unwrap();
    session.poll().unwrap();
    let reply = mdc_protocol::ConfigReply {
        key: "brightness".into(),
        ok: true,
        value: Some("99".into()),
        error: None,
    };
    let send_reply = |dev: &mut MemoryLink| {
        dev.write(
            &Packet {
                version: VERSION,
                kind: MessageType::Config,
                flags: 0,
                request_id: 0,
                payload: encode_control(&reply).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    };
    send_reply(&mut dev);
    session.poll().unwrap();
    assert_eq!(session.config("brightness"), None);
    session.get_config("brightness").unwrap();
    let request = Packet::decode(&dev.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    assert_eq!(request.kind, MessageType::Config);
    send_reply(&mut dev);
    session.poll().unwrap();
    assert_eq!(session.config("brightness"), Some("99"));
    assert_eq!(session.pending_request_count(), 0);
}

#[test]
fn write_without_pong_disconnects_on_virtual_clock() {
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::with_config(
        host,
        SessionConfig {
            connect_timeout_ms: 0,
            ack_timeout_ms: 0,
            heartbeat_ms: 10,
            reconnect: true,
        },
    );
    session.connect().unwrap();
    handshake(&mut session, &mut device);
    session.send_frame(frame(&[4; 8])).unwrap();
    let _ = device.read().unwrap();
    assert_eq!(session.tick(10).unwrap(), ConnectionState::Ready);
    let (ping, _) = Packet::decode(&device.read().unwrap().unwrap(), 4096).unwrap();
    assert_eq!(ping.kind, MessageType::Ping);
    assert_eq!(session.tick(20).unwrap(), ConnectionState::Ready);
    assert_eq!(session.tick(30).unwrap(), ConnectionState::Ready);
    assert_eq!(session.tick(40), Err(CoreError::HeartbeatMissed));
}

#[test]
fn copy_budget_counts_both_surfaces_and_releases_on_display() {
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.set_copy_limit(24);
    session.connect().unwrap();
    let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    let mut advertised = caps();
    advertised.surfaces.push(Surface {
        id: "overlay".into(),
        width: 2,
        height: 2,
        pixel_format: "RGB565".into(),
        stride: 4,
        rotation: 0,
        preferred_tile_size: 0,
        supports_partial_update: false,
        supports_full_framebuffer: false,
    });
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: hello.request_id,
                payload: encode_control(&advertised).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    let request = session.send_frame(frame(&[1; 8])).unwrap();
    assert_eq!(session.copy_outstanding(), 24);
    assert_eq!(
        session.send_frame(Frame {
            surface_id: "overlay".into(),
            width: 2,
            height: 2,
            bytes: vec![2; 8],
        }),
        Err(CoreError::CopyBudget)
    );
    assert_eq!(session.copy_outstanding(), 24);
    assert!(session.device.as_ref().unwrap().surfaces[1]
        .in_flight()
        .is_none());
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Ack,
                flags: 0,
                request_id: request,
                payload: encode_control(&Ack {
                    received: true,
                    displayed: true,
                    frame_id: 1,
                    error: None,
                    run_id: 0,
                    event_seq: 0,
                })
                .unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    assert_eq!(session.copy_outstanding(), 0);
}

#[test]
fn replayed_displayed_ack_launches_the_pending_frame_once() {
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    handshake(&mut session, &mut device);
    let request = session.send_frame(frame(&[1; 8])).unwrap();
    assert_eq!(session.send_frame(frame(&[2; 8])).unwrap(), 0);
    let _ = device.read().unwrap();
    let ack = Packet {
        version: VERSION,
        kind: MessageType::Ack,
        flags: 0,
        request_id: request,
        payload: encode_control(&Ack {
            received: true,
            displayed: true,
            frame_id: 1,
            error: None,
            run_id: 0,
            event_seq: 0,
        })
        .unwrap(),
    }
    .encode(4096)
    .unwrap();
    device.write(&ack).unwrap();
    session.poll().unwrap();
    let launched = device.read().unwrap().unwrap();
    assert!(launched.windows(8).any(|window| window == [2; 8]));
    device.write(&ack).unwrap();
    session.poll().unwrap();
    assert!(device.read().unwrap().is_none());
}

#[test]
fn input_before_the_new_handshake_is_dropped() {
    let (host, mut device) = MemoryLink::pair();
    let manager = Arc::new(Mutex::new(DeviceManager::new()));
    let mut session = Session::new(host);
    session.set_manager(manager.clone());
    session.connect().unwrap();
    handshake(&mut session, &mut device);
    let (host_b, mut device_b) = MemoryLink::pair();
    session.switch_transport(host_b).unwrap();
    let stale = Packet {
        version: VERSION,
        kind: MessageType::Input,
        flags: 0,
        request_id: 0,
        payload: encode_control(&InputEvent {
            surface_id: "main".into(),
            pointer_id: 1,
            phase: "down".into(),
            x: 0,
            y: 0,

            node: None,
        })
        .unwrap(),
    }
    .encode(4096)
    .unwrap();
    device_b.write(&stale).unwrap();
    handshake(&mut session, &mut device_b);
    let early: Vec<_> = manager.lock().unwrap().drain_events().collect();
    assert!(!early
        .iter()
        .any(|event| matches!(event, ManagerEvent::Input { .. })));
    device_b
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Input,
                flags: 0,
                request_id: 0,
                payload: encode_control(&InputEvent {
                    surface_id: "main".into(),
                    pointer_id: 2,
                    phase: "up".into(),
                    x: 1,
                    y: 1,

                    node: None,
                })
                .unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    let events: Vec<_> = manager.lock().unwrap().drain_events().collect();
    assert!(events.iter().any(|event| matches!(
        event,
        ManagerEvent::Input { event, .. } if event.phase == "up" && event.pointer_id == 2
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        ManagerEvent::Input { event, .. } if event.phase == "down"
    )));
}
#[test]
fn ota_freezes_rendering_and_rejects_missing_slot() {
    let mut unsupported = OtaController::new(false);
    assert_eq!(unsupported.begin(), Err(CoreError::Unsupported));
    let mut ota = OtaController::new(true);
    ota.begin().unwrap();
    ota.verified().unwrap();
    assert!(ota.render_frozen);
    ota.reboot().unwrap();
    ota.confirm(false);
    assert_eq!(ota.state, OtaState::Failed);
    assert!(!ota.render_frozen);
}
#[test]
fn scene_api_stays_unsupported_on_v1() {
    let (link, _) = MemoryLink::pair();
    let mut session = Session::new(link);
    assert_eq!(session.send_scene(&[1, 2, 3]), Err(CoreError::Unsupported));
}
#[test]
fn frame_transaction_accepts_out_of_order_and_idempotent_chunks() {
    let mut tx = FrameAssembler::begin(7, 8, 4).unwrap();
    tx.chunk(4, &[4, 5, 6, 7]).unwrap();
    tx.chunk(0, &[0, 1, 2, 3]).unwrap();
    tx.chunk(0, &[0, 1, 2, 3]).unwrap();
    assert_eq!(tx.commit().unwrap(), vec![0, 1, 2, 3, 4, 5, 6, 7]);
}
#[test]
fn frame_transaction_rejects_conflicts_and_incomplete_commit() {
    let mut tx = FrameAssembler::begin(1, 8, 8).unwrap();
    tx.chunk(0, &[1, 2, 3, 4]).unwrap();
    assert_eq!(
        tx.chunk(2, &[9, 9]),
        Err(TransactionNack::ConflictingDuplicate)
    );
    assert_eq!(tx.chunk(7, &[1, 2]), Err(TransactionNack::InvalidRange));
    assert_eq!(tx.commit(), Err(TransactionNack::Incomplete));
}
#[test]
fn tile_requires_current_base_frame() {
    let tile = Tile {
        surface_id: "main".into(),
        base_frame_id: 4,
        x: 0,
        y: 0,
        width: 1,
        height: 1,
        bytes: vec![0, 0],
    };
    assert_eq!(
        validate_tile_base(&tile, 3),
        Err(TransactionNack::BaseFrameMismatch)
    );
    assert_eq!(validate_tile_base(&tile, 4), Ok(()));
}
#[test]
fn manager_merges_stable_identity_and_bounds_events() {
    let caps = Capabilities {
        device_id: "device".into(),
        board_id: String::new(),
        protocol_version: 0,
        firmware: "1".into(),
        surfaces: vec![
            s(),
            Surface {
                id: "aux".into(),
                ..s()
            },
        ],
        frame: true,
        tile: true,
        touch: false,
        ota: false,
        max_message: 4096,
        max_chunk: 16384,
        max_in_flight: 1,
        max_fps: 30,
        mirror: mdc_protocol::MirrorCapability::disabled(),
        local_display_fps: 0,
        mirror_fps: 0,
        mirror_bandwidth: 0,
        renderer: None,
        config: mdc_protocol::ConfigCapability::default(),
        scene: mdc_protocol::SceneCapability::default(),
        boot_id: 0,
        run_id: 0,
    };
    let mut manager = DeviceManager::new();
    manager.upsert(caps.clone()).unwrap();
    manager.upsert(caps).unwrap();
    assert_eq!(manager.devices().count(), 1);
    assert_eq!(
        manager
            .device("device")
            .unwrap()
            .capabilities
            .surfaces
            .len(),
        2
    );
    assert_eq!(manager.drain_events().count(), 2);
    assert!(manager.remove("device"));
    assert!(manager.device("device").is_none());
}

#[test]
fn moves_coalesce_and_a_full_queue_keeps_down() {
    let mut manager = DeviceManager::new();
    let point = |phase: &str, pointer: u16, x: u16| InputEvent {
        surface_id: "main".into(),
        pointer_id: pointer,
        phase: phase.into(),
        x,
        y: 1,

        node: None,
    };
    manager.push_input("dev".into(), point("move", 1, 1));
    manager.push_input("dev".into(), point("move", 1, 4));
    let coalesced: Vec<_> = manager.drain_events().collect();
    assert_eq!(coalesced.len(), 1);
    match &coalesced[0] {
        ManagerEvent::Input { event, .. } => {
            assert_eq!(event.phase, "move");
            assert_eq!(event.x, 4);
        }
        other => panic!("expected move, got {other:?}"),
    }
    for index in 0..MAX_MANAGER_EVENTS {
        manager.push_event(ManagerEvent::Connected(format!("fill-{index}")));
    }
    manager.push_input("dev".into(), point("down", 7, 2));
    let events: Vec<_> = manager.drain_events().collect();
    assert!(events.iter().any(|event| matches!(
        event,
        ManagerEvent::Error { message, .. } if message.contains("overflow")
    )));
    assert!(events.iter().any(|event| matches!(
            event,
            ManagerEvent::Input { event, .. } if event.phase == "down" && event.pointer_id == 7 && event.x == 2
        )));
    assert!(events.iter().any(|event| matches!(
        event,
        ManagerEvent::Input { event, .. } if event.phase == "cancel" && event.pointer_id == 7
    )));
}

#[test]
fn control_queue_stops_at_32_without_counting_frames() {
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    let hello = device.read().unwrap().unwrap();
    let (hello, _) = Packet::decode(&hello, 4096).unwrap();
    let caps = Capabilities {
        ota: true,
        ..caps()
    };
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: hello.request_id,
                payload: encode_control(&caps).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    for _ in 0..MAX_CONTROL_IN_FLIGHT {
        session
            .send_ota(OtaCommand {
                action: "abort".into(),
                version: None,
                size: None,
                sha256: None,
                offset: None,
                len: None,
            })
            .unwrap();
    }
    assert_eq!(
        session.send_ota(OtaCommand {
            action: "abort".into(),
            version: None,
            size: None,
            sha256: None,
            offset: None,
            len: None,
        }),
        Err(CoreError::ControlQueueFull)
    );
    assert!(session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![1; 8],
        })
        .is_ok());
}
#[test]
fn manager_rejects_missing_identity() {
    let mut manager = DeviceManager::new();
    let mut caps = Capabilities {
        device_id: "".into(),
        board_id: String::new(),
        protocol_version: 0,
        firmware: "1".into(),
        surfaces: vec![s()],
        frame: true,
        tile: true,
        touch: false,
        ota: false,
        max_message: 4096,
        max_chunk: 16384,
        max_in_flight: 1,
        max_fps: 30,
        mirror: mdc_protocol::MirrorCapability::disabled(),
        local_display_fps: 0,
        mirror_fps: 0,
        mirror_bandwidth: 0,
        renderer: None,
        config: mdc_protocol::ConfigCapability::default(),
        scene: mdc_protocol::SceneCapability::default(),
        boot_id: 0,
        run_id: 0,
    };
    assert_eq!(
        manager.upsert(caps.clone()),
        Err(CoreError::Protocol(
            "device identity/surfaces missing".into()
        ))
    );
    caps.device_id = "id".into();
    caps.surfaces.clear();
    assert!(manager.upsert(caps).is_err());
}
#[test]
fn manager_connect_session_stores_single_endpoint() {
    let mut manager = DeviceManager::new();
    assert!(manager.connect_session("dev-a", "memory:1").is_none());
    assert_eq!(manager.active_endpoint("dev-a"), Some("memory:1"));
    let prev = manager.connect_session("dev-a", "tcp:127.0.0.1:9");
    assert_eq!(prev.as_deref(), Some("memory:1"));
    assert_eq!(manager.active_endpoint("dev-a"), Some("tcp:127.0.0.1:9"));
    let events: Vec<_> = manager.drain_events().collect();
    assert!(events.iter().any(|e| matches!(
        e,
        ManagerEvent::EndpointSwitched { device_id } if device_id == "dev-a"
    )));
}

#[test]
fn switch_transport_blocks_tiles_until_full_frame_acked() {
    let (host_a, mut dev_a) = MemoryLink::pair();
    let mut session = Session::new(host_a);
    session.connect().unwrap();
    let hello = Packet::decode(&dev_a.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    dev_a
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: hello.request_id,
                payload: encode_control(&Capabilities {
                    device_id: "sw".into(),
                    board_id: String::new(),
                    protocol_version: 0,
                    firmware: "1".into(),
                    surfaces: vec![s()],
                    frame: true,
                    tile: true,
                    touch: false,
                    ota: false,
                    max_message: 4096,
                    max_chunk: 16384,
                    max_in_flight: 1,
                    max_fps: 30,
                    mirror: mdc_protocol::MirrorCapability::disabled(),
                    local_display_fps: 0,
                    mirror_fps: 0,
                    mirror_bandwidth: 0,
                    renderer: None,
                    config: mdc_protocol::ConfigCapability::default(),
                    scene: mdc_protocol::SceneCapability::default(),
                    boot_id: 0,
                    run_id: 0,
                })
                .unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();

    let (host_b, mut dev_b) = MemoryLink::pair();
    session.switch_transport(host_b).unwrap();
    assert!(session.needs_full_frame);
    let hello2 = Packet::decode(&dev_b.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    dev_b
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: hello2.request_id,
                payload: encode_control(&Capabilities {
                    device_id: "sw".into(),
                    board_id: String::new(),
                    protocol_version: 0,
                    firmware: "1".into(),
                    surfaces: vec![s()],
                    frame: true,
                    tile: true,
                    touch: false,
                    ota: false,
                    max_message: 4096,
                    max_chunk: 16384,
                    max_in_flight: 1,
                    max_fps: 30,
                    mirror: mdc_protocol::MirrorCapability::disabled(),
                    local_display_fps: 0,
                    mirror_fps: 0,
                    mirror_bandwidth: 0,
                    renderer: None,
                    config: mdc_protocol::ConfigCapability::default(),
                    scene: mdc_protocol::SceneCapability::default(),
                    boot_id: 0,
                    run_id: 0,
                })
                .unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    assert_eq!(
        session.send_tile(Tile {
            surface_id: "main".into(),
            base_frame_id: 1,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            bytes: vec![0, 0],
        }),
        Err(CoreError::NeedFullFrame)
    );
    let req = session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![0; 8],
        })
        .unwrap();
    let frame_pkt = Packet::decode(&dev_b.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    dev_b
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Ack,
                flags: 0,
                request_id: frame_pkt.request_id,
                payload: encode_control(&Ack {
                    received: true,
                    displayed: true,
                    frame_id: req as u64,
                    error: None,
                    run_id: 0,
                    event_seq: 0,
                })
                .unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    assert!(!session.needs_full_frame);
    assert!(session
        .send_tile(Tile {
            surface_id: "main".into(),
            base_frame_id: 1,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            bytes: vec![0, 0],
        })
        .is_ok());
}
#[test]
fn repeated_transactions_release_owned_staging() {
    for frame_id in 0..100_000u64 {
        let mut tx = FrameAssembler::begin(frame_id, 4, 4).unwrap();
        tx.chunk(0, &[1, 2, 3, 4]).unwrap();
        assert_eq!(tx.commit().unwrap(), vec![1, 2, 3, 4]);
    }
}

#[test]
fn session_handshake_reaches_ready() {
    let (host_link, mut device_link) = MemoryLink::pair();
    let mut session = Session::new(host_link);
    session.connect().unwrap();
    let hello_bytes = device_link.read().unwrap().unwrap();
    let (hello, _) = Packet::decode(&hello_bytes, 4096).unwrap();
    assert_eq!(hello.kind, MessageType::Hello);
    let caps = Capabilities {
        device_id: "sim-1".into(),
        board_id: String::new(),
        protocol_version: 0,
        firmware: "simulator".into(),
        surfaces: vec![s()],
        frame: true,
        tile: true,
        touch: false,
        ota: false,
        max_message: 4096,
        max_chunk: 16384,
        max_in_flight: 1,
        max_fps: 30,
        mirror: mdc_protocol::MirrorCapability::disabled(),
        local_display_fps: 0,
        mirror_fps: 0,
        mirror_bandwidth: 0,
        renderer: None,
        config: mdc_protocol::ConfigCapability::default(),
        scene: mdc_protocol::SceneCapability::default(),
        boot_id: 0,
        run_id: 0,
    };
    let reply = Packet {
        version: VERSION,
        kind: MessageType::Capabilities,
        flags: 0,
        request_id: hello.request_id,
        payload: encode_control(&caps).unwrap(),
    };
    device_link.write(&reply.encode(4096).unwrap()).unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    assert_eq!(
        session.device.as_ref().unwrap().capabilities.device_id,
        "sim-1"
    );
    assert_eq!(
        session.device.as_ref().unwrap().state,
        ConnectionState::Ready
    );
    let frame_id = session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![0; 8],
        })
        .unwrap();
    assert_eq!(frame_id, 2);
    let frame_bytes = device_link.read().unwrap().unwrap();
    let (frame, _) = Packet::decode(&frame_bytes, MAX_FRAME_BYTES).unwrap();
    assert_eq!(frame.kind, MessageType::Frame);
    assert_eq!(session.pending_request_count(), 1);
    assert_eq!(
        session.device.as_ref().unwrap().surfaces[0].in_flight(),
        Some(1)
    );
    device_link
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Ack,
                flags: 0,
                request_id: frame.request_id,
                payload: encode_control(&Ack {
                    received: true,
                    displayed: true,
                    frame_id: 1,
                    error: None,
                    run_id: 0,
                    event_seq: 0,
                })
                .unwrap(),
            }
            .encode(MAX_FRAME_BYTES)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    assert_eq!(session.pending_request_count(), 0);
    assert!(session.device.as_ref().unwrap().surfaces[0]
        .in_flight()
        .is_none());
    session.disconnect();
    assert_eq!(session.state, ConnectionState::Disconnected);
}

#[test]
fn session_tick_heartbeat_and_ack_timeout() {
    let (host_link, mut device_link) = MemoryLink::pair();
    let mut session = Session::with_config(
        host_link,
        SessionConfig {
            connect_timeout_ms: 100,
            ack_timeout_ms: 50,
            heartbeat_ms: 30,
            reconnect: true,
        },
    );
    session.connect().unwrap();
    let hello_bytes = device_link.read().unwrap().unwrap();
    let (hello, _) = Packet::decode(&hello_bytes, 4096).unwrap();
    let caps = Capabilities {
        device_id: "sim-tick".into(),
        board_id: String::new(),
        protocol_version: 0,
        firmware: "simulator".into(),
        surfaces: vec![s()],
        frame: true,
        tile: true,
        touch: false,
        ota: false,
        max_message: 4096,
        max_chunk: 16384,
        max_in_flight: 1,
        max_fps: 30,
        mirror: mdc_protocol::MirrorCapability::disabled(),
        local_display_fps: 0,
        mirror_fps: 0,
        mirror_bandwidth: 0,
        renderer: None,
        config: mdc_protocol::ConfigCapability::default(),
        scene: mdc_protocol::SceneCapability::default(),
        boot_id: 0,
        run_id: 0,
    };
    device_link
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: hello.request_id,
                payload: encode_control(&caps).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    session.tick(0).unwrap();
    assert_eq!(session.tick(30).unwrap(), ConnectionState::Ready);
    let ping = device_link.read().unwrap().unwrap();
    let (pkt, _) = Packet::decode(&ping, 4096).unwrap();
    assert_eq!(pkt.kind, MessageType::Ping);

    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![0; 8],
        })
        .unwrap();
    let _ = device_link.read().unwrap();
    assert_eq!(session.tick(30 + 50), Err(CoreError::AckTimeout));
    assert_eq!(session.state, ConnectionState::Reconnecting);
    assert_eq!(
        session.tick(30 + 50 + 1_000).unwrap(),
        ConnectionState::Connecting
    );
    assert!(session.awaiting_transport);
}

#[test]
fn session_provide_transport_rehandshakes_after_reconnect() {
    let (host_link, mut device_link) = MemoryLink::pair();
    let mut session = Session::with_config(
        host_link,
        SessionConfig {
            ack_timeout_ms: 10,
            heartbeat_ms: 0,
            connect_timeout_ms: 1_000,
            reconnect: true,
        },
    );
    session.reconnect_policy = ReconnectPolicy {
        max_attempts: 3,
        base_delay_ms: 100,
        max_delay_ms: 100,
        jitter_ms: 0,
    };
    session.connect().unwrap();
    let hello = device_link.read().unwrap().unwrap();
    let (hello_pkt, _) = Packet::decode(&hello, 4096).unwrap();
    device_link
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: hello_pkt.request_id,
                payload: encode_control(&Capabilities {
                    device_id: "re".into(),
                    board_id: String::new(),
                    protocol_version: 0,
                    firmware: "1".into(),
                    surfaces: vec![s()],
                    frame: true,
                    tile: true,
                    touch: false,
                    ota: false,
                    max_message: 4096,
                    max_chunk: 16384,
                    max_in_flight: 1,
                    max_fps: 30,
                    mirror: mdc_protocol::MirrorCapability::disabled(),
                    local_display_fps: 0,
                    mirror_fps: 0,
                    mirror_bandwidth: 0,
                    renderer: None,
                    config: mdc_protocol::ConfigCapability::default(),
                    scene: mdc_protocol::SceneCapability::default(),
                    boot_id: 0,
                    run_id: 0,
                })
                .unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![0; 8],
        })
        .unwrap();
    let _ = device_link.read();
    assert!(session.tick(20).is_err());
    assert_eq!(session.tick(120).unwrap(), ConnectionState::Connecting);
    assert!(session.awaiting_transport);

    let (host2, mut device2) = MemoryLink::pair();
    session.provide_transport(host2).unwrap();
    let hello2 = device2.read().unwrap().unwrap();
    let (hello2_pkt, _) = Packet::decode(&hello2, 4096).unwrap();
    assert_eq!(hello2_pkt.kind, MessageType::Hello);
    device2
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: hello2_pkt.request_id,
                payload: encode_control(&Capabilities {
                    device_id: "re".into(),
                    board_id: String::new(),
                    protocol_version: 0,
                    firmware: "1".into(),
                    surfaces: vec![s()],
                    frame: true,
                    tile: true,
                    touch: false,
                    ota: false,
                    max_message: 4096,
                    max_chunk: 16384,
                    max_in_flight: 1,
                    max_fps: 30,
                    mirror: mdc_protocol::MirrorCapability::disabled(),
                    local_display_fps: 0,
                    mirror_fps: 0,
                    mirror_bandwidth: 0,
                    renderer: None,
                    config: mdc_protocol::ConfigCapability::default(),
                    scene: mdc_protocol::SceneCapability::default(),
                    boot_id: 0,
                    run_id: 0,
                })
                .unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    assert!(!session.awaiting_transport);
}

#[test]
fn dirty_tiles_merge_and_send_only_the_changed_area() {
    // N516: overlapping dirty rects collapse to one tile.
    assert_eq!(
        plan_dirty_tiles((8, 8), 4, &[(0, 0, 2, 2), (2, 0, 2, 2)]),
        vec![(0, 0, 4, 4)]
    );
    // A tile-sized rect maps to exactly one tile; a full-surface rect to all.
    assert_eq!(
        plan_dirty_tiles((8, 8), 4, &[(4, 4, 4, 4)]),
        vec![(4, 4, 4, 4)]
    );
    assert_eq!(plan_dirty_tiles((8, 8), 4, &[(0, 0, 8, 8)]).len(), 4);
    // Zero-area and out-of-range rects are dropped/clamped, and the edge
    // tiles are trimmed to the surface.
    assert!(plan_dirty_tiles((8, 8), 4, &[(0, 0, 0, 4)]).is_empty());
    // Tiles come out in tile-grid order (column-major), edges trimmed.
    assert_eq!(
        plan_dirty_tiles((6, 6), 4, &[(0, 0, 6, 6)]),
        vec![(0, 0, 4, 4), (0, 4, 4, 2), (4, 0, 2, 4), (4, 4, 2, 2)]
    );

    // The tile bytes are copied out of the frame at the right offset.
    let mut frame = vec![0u8; 8 * 8 * 2];
    for (i, byte) in frame.iter_mut().enumerate() {
        *byte = (i % 251) as u8;
    }
    let tile = extract_tile(&frame, (8, 8), (4, 4, 4, 4));
    assert_eq!(tile.len(), 4 * 4 * 2);
    // Row 0 of the tile starts at (4, 4): byte index (4*8 + 4) * 2 = 72.
    assert_eq!(&tile[..2], &frame[72..74]);

    // End to end: only the dirty tiles are written to the device.
    let (host_link, mut device_link) = MemoryLink::pair();
    let mut session = Session::new(host_link);
    session.connect().unwrap();
    let hello_bytes = device_link.read().unwrap().unwrap();
    let (hello, _) = Packet::decode(&hello_bytes, 4096).unwrap();
    let caps = Capabilities {
        device_id: "sim-dirty".into(),
        board_id: String::new(),
        protocol_version: 0,
        firmware: "simulator".into(),
        surfaces: vec![Surface {
            id: "main".into(),
            width: 8,
            height: 8,
            pixel_format: "RGB565".into(),
            stride: 16,
            rotation: 0,
            preferred_tile_size: 4,
            supports_partial_update: true,
            supports_full_framebuffer: false,
        }],
        frame: true,
        tile: true,
        touch: false,
        ota: false,
        max_message: 4096,
        max_chunk: 16384,
        max_in_flight: 1,
        max_fps: 30,
        mirror: mdc_protocol::MirrorCapability::disabled(),
        local_display_fps: 0,
        mirror_fps: 0,
        mirror_bandwidth: 0,
        renderer: None,
        config: mdc_protocol::ConfigCapability::default(),
        scene: mdc_protocol::SceneCapability::default(),
        boot_id: 0,
        run_id: 0,
    };
    device_link
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: hello.request_id,
                payload: encode_control(&caps).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    let sent = session
        .send_dirty_tiles(
            "main",
            1,
            &frame,
            (8, 8),
            4,
            &[(0, 0, 2, 2), (2, 0, 2, 2), (0, 0, 1, 1)],
        )
        .unwrap_or_else(|e| panic!("send_dirty_tiles failed: {e:?}"));
    assert_eq!(sent, 1, "three overlapping rects collapse to one tile");
    assert_eq!(session.stats().tiles_sent, 1);
    assert_eq!(session.stats().tile_bytes_sent, 32);
}

#[test]
fn tile_counters_reconcile_with_the_device() {
    // N536: the host's sent tiles are compared with the device's count.
    let stats = SessionStats {
        tiles_sent: 3,
        tile_bytes_sent: 96,
        ..SessionStats::default()
    };
    let report = reconcile_tiles(&stats, 3);
    assert!(report.consistent);
    assert_eq!(report.missing(), 0);
    assert_eq!(report.bytes_sent, 96);
    // A dropped tile is reported as missing.
    let report = reconcile_tiles(&stats, 2);
    assert!(!report.consistent);
    assert_eq!(report.missing(), 1);
    // A device that reports more than the host sent is inconsistent too.
    let report = reconcile_tiles(&SessionStats::default(), 1);
    assert!(!report.consistent);
    assert_eq!(report.missing(), 0);
}

#[test]
fn dirty_tracker_accumulates_and_escalates() {
    // N526: dirty regions accumulate across frames and switch to a full frame
    // once they cover too much of the surface.
    let mut tracker = DirtyTracker::new((8, 8), 4, 0.5);
    assert_eq!(tracker.decide(), UpdateDecision::Partial(0));
    tracker.mark((0, 0, 1, 1));
    assert_eq!(tracker.decide(), UpdateDecision::Partial(1));
    assert!((tracker.coverage() - 0.25).abs() < 1e-9);
    // A second (adjacent-in-grid) region adds another tile.
    tracker.mark((4, 0, 1, 1));
    assert_eq!(tracker.decide(), UpdateDecision::Partial(2));
    // A third pushes coverage past the threshold -> a full frame.
    tracker.mark((0, 4, 1, 1));
    assert_eq!(tracker.decide(), UpdateDecision::Full);
    assert!(tracker.tiles().is_empty(), "a full frame has no tiles");
    assert_eq!(tracker.take(), UpdateDecision::Full);
    assert!(tracker.pending().is_empty());
    assert_eq!(tracker.updates(), 1);
    // Marking everything is always a full frame.
    tracker.mark_all();
    assert_eq!(tracker.decide(), UpdateDecision::Full);
    tracker.take();
    // Overlapping rectangles collapse before the threshold is applied.
    let mut tight = DirtyTracker::new((8, 8), 4, 0.9);
    tight.mark((0, 0, 2, 2));
    tight.mark((1, 1, 2, 2));
    assert_eq!(tight.decide(), UpdateDecision::Partial(1));
}

#[test]
fn send_dirty_update_chooses_tiles_or_a_frame() {
    // N526: the session sends the tracker's decision.
    let (host_link, mut device_link) = MemoryLink::pair();
    let mut session = Session::new(host_link);
    session.connect().unwrap();
    let hello_bytes = device_link.read().unwrap().unwrap();
    let (hello, _) = Packet::decode(&hello_bytes, 4096).unwrap();
    let caps = Capabilities {
        device_id: "sim-plan".into(),
        board_id: String::new(),
        protocol_version: 0,
        firmware: "simulator".into(),
        surfaces: vec![Surface {
            id: "main".into(),
            width: 8,
            height: 8,
            pixel_format: "RGB565".into(),
            stride: 16,
            rotation: 0,
            preferred_tile_size: 4,
            supports_partial_update: true,
            supports_full_framebuffer: false,
        }],
        frame: true,
        tile: true,
        touch: false,
        ota: false,
        max_message: 4096,
        max_chunk: 16384,
        max_in_flight: 4,
        max_fps: 30,
        mirror: mdc_protocol::MirrorCapability::disabled(),
        local_display_fps: 0,
        mirror_fps: 0,
        mirror_bandwidth: 0,
        renderer: None,
        config: mdc_protocol::ConfigCapability::default(),
        scene: mdc_protocol::SceneCapability::default(),
        boot_id: 0,
        run_id: 0,
    };
    device_link
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: hello.request_id,
                payload: encode_control(&caps).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    let frame = vec![0u8; 8 * 8 * 2];
    let mut tracker = DirtyTracker::new((8, 8), 4, 0.5);
    // A small change goes out as one tile.
    tracker.mark((0, 0, 1, 1));
    assert_eq!(
        session
            .send_dirty_update("main", 0, &frame, &mut tracker)
            .unwrap(),
        UpdateDecision::Partial(1)
    );
    assert_eq!(session.stats().tiles_sent, 1);
    // Nothing dirty sends nothing.
    assert_eq!(
        session
            .send_dirty_update("main", 0, &frame, &mut tracker)
            .unwrap(),
        UpdateDecision::Partial(0)
    );
    // A large change escalates to a full frame.
    tracker.mark_all();
    assert_eq!(
        session
            .send_dirty_update("main", 0, &frame, &mut tracker)
            .unwrap(),
        UpdateDecision::Full
    );
    assert_eq!(session.stats().frames_sent, 1);
    assert_eq!(tracker.updates(), 2, "the empty update does not count");
}

#[test]
fn adaptive_dirty_threshold_follows_the_measured_link() {
    // N732: the same dirty area is tiled on a fast link and sent whole on a
    // link that cannot sustain even one frame per second.
    let dirty = || {
        let mut tracker = DirtyTracker::new((32, 32), 8, 0.5);
        for tile in 0..4 {
            tracker.mark((tile * 8, 0, 8, 8));
        }
        tracker
    };
    let frame_bytes = 32 * 32 * 2;
    let mut slow = dirty();
    assert_eq!(slow.adapt_to_bandwidth(1_000, frame_bytes), 0.05);
    assert_eq!(slow.decide(), UpdateDecision::Full);
    let mut fast = dirty();
    assert_eq!(fast.adapt_to_bandwidth(1_000_000, frame_bytes), 0.9);
    assert_eq!(fast.decide(), UpdateDecision::Partial(4));
    // A zero-sized frame leaves the threshold untouched.
    let mut unchanged = DirtyTracker::new((8, 8), 4, 0.4);
    assert_eq!(unchanged.adapt_to_bandwidth(100, 0), 0.4);
}

#[test]
fn session_measures_bitrate_and_adapts() {
    // N732: the session derives the rate from its own outbound byte counter
    // and the virtual clock, then applies it to a tracker.
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    handshake(&mut session, &mut device);
    session.tick(0).unwrap();
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![0x11; 8],
        })
        .unwrap();
    session.tick(1_000).unwrap();
    let rate = session.measure_bitrate().expect("one second elapsed");
    assert!(rate > 0, "the frame was written");
    let mut tracker = DirtyTracker::new((32, 32), 8, 0.5);
    let threshold = session
        .adapt_dirty_threshold(&mut tracker, 32 * 32 * 2)
        .expect("rate available");
    assert!((0.05..=0.9).contains(&threshold));
    // A second sample with no time elapsed has no rate.
    assert_eq!(session.measure_bitrate(), None);
}

#[test]
fn session_adapts_frame_pacing_to_the_measured_rate() {
    // N741: a low measured rate caps the scheduler at one frame per second.
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    handshake(&mut session, &mut device);
    session.set_frame_pacing(30, 0);
    session.tick(0).unwrap();
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![0x11; 8],
        })
        .unwrap();
    session.tick(1_000).unwrap();
    let config = session
        .adapt_frame_pacing(10_000_000)
        .expect("scheduler and rate present");
    assert_eq!(config.max_fps, 1);
    assert!(config.max_bytes_per_second > 0);
}

#[test]
fn joint_streaming_tuning_updates_both_knobs() {
    // N750: one measured rate drives both pacing and dirty thresholds.
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    handshake(&mut session, &mut device);
    session.set_frame_pacing(30, 0);
    session.tick(0).unwrap();
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![0x11; 8],
        })
        .unwrap();
    session.tick(1_000).unwrap();
    let mut tracker = DirtyTracker::new((32, 32), 8, 0.5);
    for tile in 0..4 {
        tracker.mark((tile * 8, 0, 8, 8));
    }
    let tuning = session
        .tune_streaming(&mut tracker, 10_000_000)
        .expect("rate present");
    assert!(tuning.bitrate > 0);
    assert_eq!(tuning.pacing.unwrap().max_fps, 1);
    assert_eq!(tuning.dirty_threshold, Some(0.05));
    assert_eq!(tracker.decide(), UpdateDecision::Full);
}

#[test]
fn session_send_tile_writes_tile_packet() {
    let (host_link, mut device_link) = MemoryLink::pair();
    let mut session = Session::new(host_link);
    session.connect().unwrap();
    let hello_bytes = device_link.read().unwrap().unwrap();
    let (hello, _) = Packet::decode(&hello_bytes, 4096).unwrap();
    let caps = Capabilities {
        device_id: "sim-tile".into(),
        board_id: String::new(),
        protocol_version: 0,
        firmware: "simulator".into(),
        surfaces: vec![s()],
        frame: true,
        tile: true,
        touch: false,
        ota: false,
        max_message: 4096,
        max_chunk: 16384,
        max_in_flight: 1,
        max_fps: 30,
        mirror: mdc_protocol::MirrorCapability::disabled(),
        local_display_fps: 0,
        mirror_fps: 0,
        mirror_bandwidth: 0,
        renderer: None,
        config: mdc_protocol::ConfigCapability::default(),
        scene: mdc_protocol::SceneCapability::default(),
        boot_id: 0,
        run_id: 0,
    };
    device_link
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: hello.request_id,
                payload: encode_control(&caps).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    session
        .send_tile(Tile {
            surface_id: "main".into(),
            base_frame_id: 1,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            bytes: vec![0x11, 0x22],
        })
        .unwrap();
    let bytes = device_link.read().unwrap().unwrap();
    let (pkt, _) = Packet::decode(&bytes, 4096).unwrap();
    assert_eq!(pkt.kind, MessageType::Tile);
    let tile = mdc_protocol::decode_tile_payload(&pkt.payload).unwrap();
    assert_eq!(tile.bytes, vec![0x11, 0x22]);
    // N506: the tile and its bytes are counted.
    let stats = session.stats();
    assert_eq!(stats.tiles_sent, 1);
    assert_eq!(stats.tile_bytes_sent, 2);
    session
        .send_tile(Tile {
            surface_id: "main".into(),
            base_frame_id: 1,
            x: 0,
            y: 0,
            width: 2,
            height: 1,
            bytes: vec![0x33, 0x44, 0x55, 0x66],
        })
        .unwrap();
    let stats = session.stats();
    assert_eq!(stats.tiles_sent, 2);
    assert_eq!(stats.tile_bytes_sent, 6);
    // A tile for an unknown surface fails before it is counted.
    assert!(session
        .send_tile(Tile {
            surface_id: "nope".into(),
            base_frame_id: 1,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            bytes: vec![0x00, 0x00],
        })
        .is_err());
    assert_eq!(session.stats().tiles_sent, 2);
}

#[test]
fn device_tiles_received_folds_into_the_stats_snapshot() {
    // N547: the device's reported applied-tile count is part of one snapshot,
    // and the outstanding gap is derived there.
    let (host_link, mut device_link) = MemoryLink::pair();
    let mut session = Session::new(host_link);
    session.connect().unwrap();
    let hello_bytes = device_link.read().unwrap().unwrap();
    let (hello, _) = Packet::decode(&hello_bytes, 4096).unwrap();
    write_caps(&mut device_link, hello.request_id, &caps());
    session.poll().unwrap();

    session
        .send_tile(Tile {
            surface_id: "main".into(),
            base_frame_id: 1,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            bytes: vec![0x11, 0x22],
        })
        .unwrap();
    assert_eq!(session.stats().tiles_sent, 1);
    assert_eq!(session.stats().device_tiles_received, 0, "nothing reported");
    assert_eq!(session.stats().tiles_missing, 1);

    // The device reports it applied the tile.
    let metric = MetricReport {
        name: TILES_RECEIVED_METRIC.into(),
        value: 1.0,
        unit: Some("tiles".into()),
        boot_id: 0,
        run_id: 0,
        event_seq: 1,
    };
    device_link
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Metric,
                flags: 0,
                request_id: 0,
                payload: encode_control(&metric).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    assert_eq!(session.stats().device_tiles_received, 1);
    assert_eq!(session.stats().tiles_missing, 0);
    let report = session.tile_reconciliation();
    assert_eq!((report.sent, report.received), (1, 1));
    assert_eq!(report.missing(), 0);
    assert!(report.consistent);
}

#[test]
fn tile_reconciliation_threshold_forces_a_full_frame() {
    // N557: too many unacknowledged tiles set the full-frame barrier.
    let (host_link, mut device_link) = MemoryLink::pair();
    let mut session = Session::new(host_link);
    session.connect().unwrap();
    let hello_bytes = device_link.read().unwrap().unwrap();
    let (hello, _) = Packet::decode(&hello_bytes, 4096).unwrap();
    write_caps(&mut device_link, hello.request_id, &caps());
    session.poll().unwrap();
    for (x, y) in [(0u16, 0u16), (1, 0), (0, 1)] {
        session
            .send_tile(Tile {
                surface_id: "main".into(),
                base_frame_id: 1,
                x,
                y,
                width: 1,
                height: 1,
                bytes: vec![0x11, 0x22],
            })
            .unwrap();
    }
    let metric = MetricReport {
        name: TILES_RECEIVED_METRIC.into(),
        value: 1.0,
        unit: None,
        boot_id: 0,
        run_id: 0,
        event_seq: 1,
    };
    device_link
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Metric,
                flags: 0,
                request_id: 0,
                payload: encode_control(&metric).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    assert_eq!(session.tile_reconciliation().missing(), 2);
    // A generous threshold keeps tiling; a tight one forces a full frame.
    assert!(!session.tile_reconciliation().needs_full_frame(5));
    let report = session.enforce_tile_reconciliation(1);
    assert!(report.needs_full_frame(1));
    assert!(session.needs_full_frame);
    // The check itself is side-effect free when it passes.
    session.needs_full_frame = false;
    session.enforce_tile_reconciliation(5);
    assert!(!session.needs_full_frame);
}

#[test]
fn connect_disconnect_loop_is_cheap() {
    for _ in 0..1000 {
        let (host_link, mut device_link) = MemoryLink::pair();
        let mut session = Session::new(host_link);
        session.connect().unwrap();
        let hello_bytes = device_link.read().unwrap().unwrap();
        let (hello, _) = Packet::decode(&hello_bytes, 4096).unwrap();
        let caps = Capabilities {
            device_id: "loop".into(),
            board_id: String::new(),
            protocol_version: 0,
            firmware: "simulator".into(),
            surfaces: vec![s()],
            frame: true,
            tile: true,
            touch: false,
            ota: false,
            max_message: 4096,
            max_chunk: 16384,
            max_in_flight: 1,
            max_fps: 30,
            mirror: mdc_protocol::MirrorCapability::disabled(),
            local_display_fps: 0,
            mirror_fps: 0,
            mirror_bandwidth: 0,
            renderer: None,
            config: mdc_protocol::ConfigCapability::default(),
            scene: mdc_protocol::SceneCapability::default(),
            boot_id: 0,
            run_id: 0,
        };
        device_link
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Capabilities,
                    flags: 0,
                    request_id: hello.request_id,
                    payload: encode_control(&caps).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
        session.disconnect();
        assert_eq!(session.state, ConnectionState::Disconnected);
    }
}

#[test]
fn session_chunks_when_payload_exceeds_max_chunk() {
    let (host_link, mut device_link) = MemoryLink::pair();
    let mut session = Session::new(host_link);
    session.connect().unwrap();
    let hello_bytes = device_link.read().unwrap().unwrap();
    let (hello, _) = Packet::decode(&hello_bytes, 4096).unwrap();
    let caps = Capabilities {
        device_id: "sim-chunk".into(),
        board_id: String::new(),
        protocol_version: 0,
        firmware: "simulator".into(),
        surfaces: vec![Surface {
            id: "main".into(),
            width: 4,
            height: 2,
            pixel_format: "RGB565".into(),
            stride: 8,
            rotation: 0,
            preferred_tile_size: 0,
            supports_partial_update: false,
            supports_full_framebuffer: false,
        }],
        frame: true,
        tile: true,
        touch: false,
        ota: false,
        max_message: 4096,
        max_chunk: 8,
        max_in_flight: 1,
        max_fps: 30,
        mirror: mdc_protocol::MirrorCapability::disabled(),
        local_display_fps: 0,
        mirror_fps: 0,
        mirror_bandwidth: 0,
        renderer: None,
        config: mdc_protocol::ConfigCapability::default(),
        scene: mdc_protocol::SceneCapability::default(),
        boot_id: 0,
        run_id: 0,
    };
    device_link
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: hello.request_id,
                payload: encode_control(&caps).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    // 4*2*2 = 16 pixel bytes; legacy payload > max_chunk=8
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 4,
            height: 2,
            bytes: (0..16).collect(),
        })
        .unwrap();
    let mut phases = Vec::new();
    while let Some(bytes) = device_link.read().unwrap() {
        let (pkt, _) = Packet::decode(&bytes, 4096).unwrap();
        phases.push(pkt.payload[0]);
    }
    assert!(phases.contains(&mdc_protocol::FRAME_PHASE_BEGIN));
    assert!(phases.contains(&mdc_protocol::FRAME_PHASE_CHUNK));
    assert!(phases.contains(&mdc_protocol::FRAME_PHASE_COMMIT));
}

#[test]
fn latest_unsent_frame_replaces_pending_slot() {
    let mut queue = SurfaceQueue::new(s());
    let frame = |value: u8| Frame {
        surface_id: "main".into(),
        width: 2,
        height: 2,
        bytes: vec![value; 8],
    };
    assert_eq!(queue.enqueue_frame(frame(1)).unwrap(), 1);
    assert_eq!(queue.in_flight(), Some(1));
    assert_eq!(queue.enqueue_frame(frame(2)).unwrap(), 2);
    assert_eq!(queue.pending_frame_id(), Some(2));
    assert_eq!(queue.enqueue_frame(frame(3)).unwrap(), 3);
    assert_eq!(queue.in_flight(), Some(1));
    assert_eq!(queue.pending_frame_id(), Some(3));
    let (launched_id, launched) = queue.ack(1).unwrap();
    assert_eq!(launched_id, 3);
    assert_eq!(launched.bytes, vec![3; 8]);
    assert_eq!(queue.in_flight(), Some(3));
}

#[test]
fn received_ack_holds_the_next_frame_until_displayed() {
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    let hello = device.read().unwrap().unwrap();
    let (hello, _) = Packet::decode(&hello, 4096).unwrap();
    let caps = caps();
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: hello.request_id,
                payload: encode_control(&caps).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    session.needs_full_frame = true;
    let first = session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![0x11; 8],
        })
        .unwrap();
    let _ = device.read().unwrap().unwrap();
    assert_eq!(
        session.send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![0x22; 8],
        }),
        Ok(0)
    );
    assert!(device.read().unwrap().is_none());
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Ack,
                flags: 0,
                request_id: first,
                payload: encode_control(&Ack {
                    received: true,
                    displayed: false,
                    frame_id: first as u64,
                    error: None,
                    run_id: 0,
                    event_seq: 0,
                })
                .unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    assert!(device.read().unwrap().is_none());
    assert!(session.needs_full_frame);
    assert_eq!(
        session.send_tile(Tile {
            surface_id: "main".into(),
            base_frame_id: 1,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            bytes: vec![0, 0],
        }),
        Err(CoreError::NeedFullFrame)
    );
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Ack,
                flags: 0,
                request_id: first,
                payload: encode_control(&Ack {
                    received: true,
                    displayed: true,
                    frame_id: first as u64,
                    error: None,
                    run_id: 0,
                    event_seq: 0,
                })
                .unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    let launched = device.read().unwrap().unwrap();
    assert!(launched.windows(2).any(|pair| pair == [0x22, 0x22]));
    assert!(!session.needs_full_frame);
}

#[test]
fn three_missed_heartbeats_reconnect_and_block_tiles() {
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::with_config(
        host,
        SessionConfig {
            connect_timeout_ms: 10_000,
            ack_timeout_ms: 0,
            heartbeat_ms: 1_000,
            reconnect: true,
        },
    );
    session.reconnect_policy = ReconnectPolicy {
        max_attempts: 3,
        base_delay_ms: 1_000,
        max_delay_ms: 30_000,
        jitter_ms: 0,
    };
    assert_eq!(session.reconnect_policy.delay_ms(1, 0), Some(1_000));
    assert_eq!(session.reconnect_policy.delay_ms(2, 0), Some(2_000));
    assert_eq!(session.reconnect_policy.delay_ms(3, 0), Some(4_000));
    session.connect().unwrap();
    let hello = device.read().unwrap().unwrap();
    let (hello, _) = Packet::decode(&hello, 4096).unwrap();
    let caps = caps();
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: hello.request_id,
                payload: encode_control(&caps).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    session.tick(1_000).unwrap();
    assert!(device.read().unwrap().is_some());
    session.tick(2_000).unwrap();
    assert!(device.read().unwrap().is_some());
    session.tick(3_000).unwrap();
    assert!(device.read().unwrap().is_some());
    assert_eq!(session.tick(4_000), Err(CoreError::HeartbeatMissed));
    assert_eq!(session.state, ConnectionState::Reconnecting);
    assert_eq!(session.tick(5_000).unwrap(), ConnectionState::Connecting);
    let (host2, mut device2) = MemoryLink::pair();
    session.provide_transport(host2).unwrap();
    assert!(session.needs_full_frame);
    let hello = device2.read().unwrap().unwrap();
    let (hello, _) = Packet::decode(&hello, 4096).unwrap();
    device2
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: hello.request_id,
                payload: encode_control(&caps).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    assert_eq!(
        session.send_tile(Tile {
            surface_id: "main".into(),
            base_frame_id: 1,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            bytes: vec![0, 0],
        }),
        Err(CoreError::NeedFullFrame)
    );
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![7; 8],
        })
        .unwrap();
    let frame = device2.read().unwrap().unwrap();
    let (frame, _) = Packet::decode(&frame, 4096).unwrap();
    device2
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Ack,
                flags: 0,
                request_id: frame.request_id,
                payload: encode_control(&Ack {
                    received: true,
                    displayed: true,
                    frame_id: frame.request_id as u64,
                    error: None,
                    run_id: 0,
                    event_seq: 0,
                })
                .unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    session
        .send_tile(Tile {
            surface_id: "main".into(),
            base_frame_id: 1,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            bytes: vec![0, 0],
        })
        .unwrap();
}

#[test]
fn thousand_sessions_and_frames_return_the_ledger_to_zero() {
    let mut ledger = AllocationLedger::default();
    let frame_bytes = 8usize;
    for _ in 0..1000 {
        assert!(ledger.alloc(frame_bytes));
        let (host, mut link) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        let hello = link.read().unwrap().unwrap();
        let (hello, _) = Packet::decode(&hello, 4096).unwrap();
        link.write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: hello.request_id,
                payload: encode_control(&caps()).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
        session.poll().unwrap();
        session.disconnect();
        ledger.free(frame_bytes);
    }
    let (host, mut link) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    let hello = link.read().unwrap().unwrap();
    let (hello, _) = Packet::decode(&hello, 4096).unwrap();
    link.write(
        &Packet {
            version: VERSION,
            kind: MessageType::Capabilities,
            flags: 0,
            request_id: hello.request_id,
            payload: encode_control(&caps()).unwrap(),
        }
        .encode(4096)
        .unwrap(),
    )
    .unwrap();
    session.poll().unwrap();
    for value in 0..100u8 {
        assert!(ledger.alloc(frame_bytes));
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![value; frame_bytes],
            })
            .unwrap();
        let packet = link.read().unwrap().unwrap();
        let (packet, _) = Packet::decode(&packet, 4096).unwrap();
        link.write(
            &Packet {
                version: VERSION,
                kind: MessageType::Ack,
                flags: 0,
                request_id: packet.request_id,
                payload: encode_control(&Ack {
                    received: true,
                    displayed: true,
                    frame_id: packet.request_id as u64,
                    error: None,
                    run_id: 0,
                    event_seq: 0,
                })
                .unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
        session.poll().unwrap();
        ledger.free(frame_bytes);
    }
    session.disconnect();
    assert_eq!(ledger.outstanding, 0);
}

fn mirror_caps() -> Capabilities {
    let mut c = caps();
    c.mirror = mdc_protocol::MirrorCapability::full_frame(2, 2, 15);
    c.local_display_fps = 30;
    c.mirror_fps = 15;
    c.mirror_bandwidth = 2 * 2 * 2 * 15;
    c
}

fn write_caps(device: &mut MemoryLink, request_id: u32, caps: &Capabilities) {
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id,
                payload: encode_control(caps).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
}

fn mirror_packet(kind: MessageType, payload: Vec<u8>) -> Vec<u8> {
    Packet {
        version: VERSION,
        kind,
        flags: 0,
        request_id: 0,
        payload,
    }
    .encode(4096)
    .unwrap()
}

#[test]
fn disconnect_drops_the_mirror_inbox_and_base() {
    // SIM-R04/SIM-T11.
    let (host, mut dev) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    let hello = Packet::decode(&dev.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    let caps = mirror_caps();
    write_caps(&mut dev, hello.request_id, &caps);
    session.poll().unwrap();
    session.start_mirror("main", "full-frame", 30).unwrap();
    let _ = dev.read().unwrap().unwrap();
    dev.write(&mirror_packet(
        MessageType::MirrorFrame,
        encode_frame_payload("main", 1, 2, 2, &[0x11; 8]).unwrap(),
    ))
    .unwrap();
    session.poll().unwrap();
    assert!(session.mirror_inbox.is_some());
    assert!(session.mirror_last.is_some());
    session.disconnect();
    assert!(session.take_mirror_frame().is_none());
    assert!(session.mirror_inbox.is_none());
    assert!(session.mirror_last.is_none());
    assert!(session.mirror_assembler.is_none());
}

#[test]
fn mirror_start_is_rejected_without_capability() {
    let (host, mut dev) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    handshake(&mut session, &mut dev);
    assert!(!session.supports_mirror());
    assert_eq!(
        session.start_mirror("main", "full-frame", 15),
        Err(CoreError::Unsupported)
    );
    assert_eq!(session.stop_mirror("main"), Err(CoreError::Unsupported));
    assert!(dev.read().unwrap().is_none());
}

#[test]
fn dirty_region_mirror_patches_the_last_frame() {
    let (host, mut dev) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    let hello = Packet::decode(&dev.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    let mut caps = mirror_caps();
    caps.mirror.mode.push("dirty-region".into());
    write_caps(&mut dev, hello.request_id, &caps);
    session.poll().unwrap();
    session.start_mirror("main", "dirty-region", 30).unwrap();
    let ctrl = Packet::decode(&dev.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    let cmd: mdc_protocol::MirrorCommand = mdc_protocol::decode_control(&ctrl.payload).unwrap();
    assert_eq!(cmd.mode, "dirty-region");

    // Full base frame (2x2, all 0x11).
    dev.write(&mirror_packet(
        MessageType::MirrorFrame,
        encode_frame_payload("main", 1, 2, 2, &[0x11; 8]).unwrap(),
    ))
    .unwrap();
    session.poll().unwrap();
    assert_eq!(session.take_mirror_frame().unwrap().bytes, vec![0x11; 8]);

    // Dirty update: only the first row (offsets 0..4) changes to 0x22.
    dev.write(&mirror_packet(
        MessageType::MirrorFrame,
        mdc_protocol::encode_frame_begin("main", 2, 2, 2, 8).unwrap(),
    ))
    .unwrap();
    dev.write(&mirror_packet(
        MessageType::MirrorFrame,
        mdc_protocol::encode_frame_chunk(2, 0, &[0x22; 4]),
    ))
    .unwrap();
    dev.write(&mirror_packet(
        MessageType::MirrorFrame,
        mdc_protocol::encode_frame_commit(2),
    ))
    .unwrap();
    session.poll().unwrap();
    let patched = session.take_mirror_frame().expect("patched frame");
    assert_eq!(
        patched.bytes,
        vec![0x22, 0x22, 0x22, 0x22, 0x11, 0x11, 0x11, 0x11]
    );
    assert_eq!(session.mirror_frames_received(), 2);
}

#[test]
fn mirror_frames_are_bounded_to_latest() {
    let (host, mut dev) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    let hello = Packet::decode(&dev.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    write_caps(&mut dev, hello.request_id, &mirror_caps());
    session.poll().unwrap();
    assert!(session.supports_mirror());
    session.start_mirror("main", "dirty-region", 30).unwrap();
    // negotiated down to full-frame at the device ceiling.
    let ctrl = Packet::decode(&dev.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    assert_eq!(ctrl.kind, MessageType::MirrorCtrl);
    let cmd: mdc_protocol::MirrorCommand = mdc_protocol::decode_control(&ctrl.payload).unwrap();
    assert_eq!(cmd.mode, "full-frame");
    assert_eq!(cmd.max_fps, 15);

    for (frame_id, value) in [(17u64, 0x11u8), (34u64, 0x22u8)] {
        let payload = encode_frame_payload("main", frame_id, 2, 2, &[value; 8]).unwrap();
        dev.write(&mirror_packet(MessageType::MirrorFrame, payload))
            .unwrap();
        session.poll().unwrap();
    }
    let frame = session.take_mirror_frame().unwrap();
    assert_eq!(frame.frame_id, 34);
    assert_eq!(frame.bytes, vec![0x22; 8]);
    assert_eq!(session.mirror_frames_received(), 2);
    assert_eq!(session.mirror_frames_dropped(), 1);
    assert!(session.take_mirror_frame().is_none());
}

#[test]
fn mirror_frames_emit_a_metadata_event_and_stats() {
    let (host, mut dev) = MemoryLink::pair();
    let mut session = Session::new(host);
    let manager = Arc::new(Mutex::new(DeviceManager::new()));
    session.set_manager(manager.clone());
    session.connect().unwrap();
    let hello = Packet::decode(&dev.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    write_caps(&mut dev, hello.request_id, &mirror_caps());
    session.poll().unwrap();

    let payload = encode_frame_payload("main", 7, 2, 2, &[0x33; 8]).unwrap();
    dev.write(&mirror_packet(MessageType::MirrorFrame, payload))
        .unwrap();
    session.poll().unwrap();

    let events: Vec<_> = manager.lock().unwrap().drain_events().collect();
    assert!(
        events.iter().any(|event| matches!(
            event,
            ManagerEvent::Mirror {
                frame_id: 7,
                width: 2,
                height: 2,
                frames_received: 1,
                ..
            }
        )),
        "{events:?}"
    );
    let stats = session.mirror_stats();
    assert_eq!(stats.frames_received, 1);
    assert_eq!(stats.frames_dropped, 0);
    assert_eq!(stats.last_frame_id, 7);
}

#[test]
fn mirror_chunked_frame_assembles_and_rejects_bad_geometry() {
    let (host, mut dev) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    let hello = Packet::decode(&dev.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    write_caps(&mut dev, hello.request_id, &mirror_caps());
    session.poll().unwrap();

    let begin = encode_frame_begin("main", 5, 2, 2, 8).unwrap();
    dev.write(&mirror_packet(MessageType::MirrorFrame, begin))
        .unwrap();
    session.poll().unwrap();
    let chunk = encode_frame_chunk(5, 0, &[9; 8]);
    dev.write(&mirror_packet(MessageType::MirrorFrame, chunk))
        .unwrap();
    session.poll().unwrap();
    let commit = encode_frame_commit(5);
    dev.write(&mirror_packet(MessageType::MirrorFrame, commit))
        .unwrap();
    session.poll().unwrap();
    let frame = session.take_mirror_frame().unwrap();
    assert_eq!(frame.frame_id, 5);
    assert_eq!(frame.bytes, vec![9; 8]);

    // Geometry no surface advertises is dropped, not stored.
    let bad = encode_frame_payload("main", 6, 4, 2, &[1; 16]).unwrap();
    dev.write(&mirror_packet(MessageType::MirrorFrame, bad))
        .unwrap();
    session.poll().unwrap();
    assert!(session.take_mirror_frame().is_none());
    assert_eq!(session.mirror_frames_received(), 1);
}

#[test]
fn device_reboot_discards_the_previous_run() {
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    let mut first = caps();
    first.boot_id = 100;
    first.run_id = 1;
    write_caps(&mut device, hello.request_id, &first);
    session.poll().unwrap();
    assert_eq!(session.device_boot_id(), Some(100));
    assert_eq!(session.device_run_id(), 1);

    // A frame becomes in-flight, so there is a pending ACK for this run.
    let request = session.send_frame(frame(&[1; 8])).unwrap();
    assert_ne!(request, 0);
    assert_eq!(session.pending_request_count(), 1);
    assert!(!session.needs_full_frame);

    // Same boot id -> no reset.
    write_caps(&mut device, 900, &first);
    session.poll().unwrap();
    assert_eq!(session.pending_request_count(), 1);

    // New boot id -> the old run's pending ACK is dropped and a full frame
    // is required again.
    let mut second = first.clone();
    second.boot_id = 101;
    second.run_id = 2;
    write_caps(&mut device, 901, &second);
    session.poll().unwrap();
    assert_eq!(session.device_boot_id(), Some(101));
    assert_eq!(session.device_run_id(), 2);
    assert_eq!(session.pending_request_count(), 0);
    assert!(session.needs_full_frame);
}

#[test]
fn stale_run_ack_is_ignored() {
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    let mut run2 = caps();
    run2.boot_id = 7;
    run2.run_id = 2;
    write_caps(&mut device, hello.request_id, &run2);
    session.poll().unwrap();
    let request = session.send_frame(frame(&[2; 8])).unwrap();
    assert_eq!(session.pending_request_count(), 1);
    // An ACK stamped with run 1 is from the previous boot and must not clear
    // the request.
    let stale = Ack {
        received: true,
        displayed: true,
        frame_id: 1,
        error: None,
        run_id: 1,
        event_seq: 5,
    };
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Ack,
                flags: 0,
                request_id: request,
                payload: encode_control(&stale).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    assert_eq!(session.pending_request_count(), 1);
}

#[test]
fn session_stats_count_frames_and_acks() {
    // N398: the host counters track outbound packets, frames, and received acks.
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    handshake(&mut session, &mut device);
    assert_eq!(session.state, ConnectionState::Ready);
    assert!(session.stats().packets_sent >= 1, "hello counted");

    let before = session.stats();
    let request = session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![0u8; 8],
        })
        .unwrap();
    assert_eq!(session.stats().frames_sent, before.frames_sent + 1);
    assert!(session.stats().bytes_sent > before.bytes_sent);

    // The peer acknowledges (received + displayed).
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Ack,
                flags: 0,
                request_id: request,
                payload: encode_control(&mdc_protocol::Ack {
                    received: true,
                    displayed: true,
                    frame_id: 0,
                    error: None,
                    run_id: 0,
                    event_seq: 1,
                })
                .unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    assert!(session.stats().acks_received >= 1, "the frame was acked");
    // N436: inbound bytes are counted too.
    assert!(session.stats().bytes_received > 0, "received bytes counted");
    // N446: the package/mirror/pacing counters are exposed.
    let _ = (
        session.stats().packages_sent,
        session.stats().mirror_frames,
        session.stats().pacing_dropped,
    );
}

#[test]
fn set_scene_gates_on_the_animation_budget() {
    // N477: a scene with more keys than the device allows is refused.
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    let mut advertised = caps();
    advertised.scene = mdc_protocol::SceneCapability::enabled_with_animation(64, 4, 1);
    let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: hello.request_id,
                payload: encode_control(&advertised).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    assert_eq!(session.state, ConnectionState::Ready);
    let scene = |keys: u64| mdc_protocol::SceneNode {
        name: "root".into(),
        kind: "rect".into(),
        rect: Some([0, 0, 2, 2]),
        value: Some("#fff".into()),
        value2: None,
        opacity: None,
        visible: None,
        rotate: None,
        blend: None,
        align: None,
        z: None,
        wrap: None,
        mode: None,
        focus: None,
        anim: Some(mdc_protocol::SceneAnim {
            duration_ms: 100,
            looping: true,
            opacity: (0..keys).map(|k| [k as u16, 128]).collect(),
            rect: Vec::new(),
            focus: Vec::new(),
            delay_ms: None,
            speed_permille: None,
            color: Vec::new(),
            rotate: Vec::new(),
            easing: None,
            scale: Vec::new(),
            offset: Vec::new(),
            frame: Vec::new(),
        }),
        children: Vec::new(),
    };
    assert!(session.set_scene(scene(1)).is_ok(), "one key fits");
    assert_eq!(session.set_scene(scene(2)), Err(CoreError::TooLarge));
}

#[test]
fn gesture_config_tunes_the_tap_window() {
    // N488: a short tap window rejects a slow press.
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    let manager = std::sync::Arc::new(std::sync::Mutex::new(DeviceManager::new()));
    session.set_manager(manager.clone());
    session.connect().unwrap();
    handshake(&mut session, &mut device);
    session.set_gesture_config(GestureConfig {
        tap_max_ms: 10,
        long_press_ms: 5_000,
        drag_threshold: 6,
        multi_tap_ms: 300,
        multi_tap_radius: 24,
    });
    assert_eq!(session.gesture_config().tap_max_ms, 10);
    let input = |phase: &str| mdc_protocol::InputEvent {
        surface_id: "main".into(),
        pointer_id: 1,
        phase: phase.into(),
        x: 1,
        y: 1,
        node: None,
    };
    // Poll at t=0 (down), then advance past the tap window before "up".
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Input,
                flags: 0,
                request_id: 1,
                payload: encode_control(&input("down")).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    session.tick(500).unwrap();
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Input,
                flags: 0,
                request_id: 2,
                payload: encode_control(&input("up")).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    let events: Vec<ManagerEvent> = manager.lock().unwrap().drain_events().collect();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ManagerEvent::Gesture { .. })),
        "a slow press is not a tap"
    );
}

#[test]
fn multi_tap_window_from_the_setter_splits_chains() {
    // N804: the window stored by `set_gesture_config` decides whether the
    // next tap escalates. A tap inside it is a double-tap; a tap outside it
    // starts a new chain. The radius already on the session stays put.
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    let manager = std::sync::Arc::new(std::sync::Mutex::new(DeviceManager::new()));
    session.set_manager(manager.clone());
    session.connect().unwrap();
    handshake(&mut session, &mut device);
    let mut config = session.gesture_config();
    let radius = config.multi_tap_radius;
    config.multi_tap_ms = 100;
    session.set_gesture_config(config);
    assert_eq!(session.gesture_config().multi_tap_ms, 100);
    assert_eq!(session.gesture_config().multi_tap_radius, radius);
    let input = |phase: &str| mdc_protocol::InputEvent {
        surface_id: "main".into(),
        pointer_id: 1,
        phase: phase.into(),
        x: 4,
        y: 5,
        node: None,
    };
    let mut request = 1u32;
    let mut tap_at = |at: u64| {
        session.tick(at).unwrap();
        for phase in ["down", "up"] {
            device
                .write(
                    &Packet {
                        version: VERSION,
                        kind: MessageType::Input,
                        flags: 0,
                        request_id: request,
                        payload: encode_control(&input(phase)).unwrap(),
                    }
                    .encode(4096)
                    .unwrap(),
                )
                .unwrap();
            request += 1;
        }
        session.poll().unwrap();
        let events: Vec<ManagerEvent> = manager.lock().unwrap().drain_events().collect();
        events.into_iter().find_map(|event| match event {
            ManagerEvent::Gesture { gesture, .. } => Some(gesture.kind),
            _ => None,
        })
    };
    assert_eq!(tap_at(0), Some(GestureKind::Tap));
    assert_eq!(tap_at(100), Some(GestureKind::DoubleTap));
    // N814: a third tap inside the stored window is a triple-tap.
    assert_eq!(tap_at(200), Some(GestureKind::TripleTap));
    // The triple resets the streak; a tap outside the window restarts at 1.
    assert_eq!(tap_at(350), Some(GestureKind::Tap));
    assert_eq!(tap_at(430), Some(GestureKind::DoubleTap));
    assert_eq!(tap_at(600), Some(GestureKind::Tap));
    assert_eq!(tap_at(680), Some(GestureKind::DoubleTap));
}

#[test]
fn gestures_reach_the_manager_event_queue() {
    // N473: down+up inside the tap window becomes a `Gesture` event.
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    let manager = std::sync::Arc::new(std::sync::Mutex::new(DeviceManager::new()));
    session.set_manager(manager.clone());
    session.connect().unwrap();
    handshake(&mut session, &mut device);
    let input = |phase: &str| mdc_protocol::InputEvent {
        surface_id: "main".into(),
        pointer_id: 3,
        phase: phase.into(),
        x: 4,
        y: 5,
        // N493: the device reports the hit node with the event.
        node: Some("btn".into()),
    };
    for (phase, id) in [("down", 10u32), ("up", 11)] {
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Input,
                    flags: 0,
                    request_id: id,
                    payload: encode_control(&input(phase)).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
    }
    session.poll().unwrap();
    let events: Vec<ManagerEvent> = manager.lock().unwrap().drain_events().collect();
    assert!(events.iter().any(|event| matches!(
        event,
        ManagerEvent::Gesture {
            gesture: Gesture {
                kind: GestureKind::Tap,
                pointer_id: 3,
                x: 4,
                y: 5,
                node: Some(node),
                ..
            },
            ..
        } if node == "btn"
    )));
}

#[test]
fn send_input_requires_the_touch_capability() {
    // N400: `caps()` advertises touch: false, so the send is refused.
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    handshake(&mut session, &mut device);
    let event = mdc_protocol::InputEvent {
        surface_id: "main".into(),
        pointer_id: 1,
        phase: "down".into(),
        x: 1,
        y: 2,

        node: None,
    };
    assert_eq!(session.send_input(event), Err(CoreError::Unsupported));
}

#[test]
fn gesture_tracker_recognises_tap_drag_and_long_press() {
    // N464: tap, drag, and long-press derive from raw input events.
    let event = |phase: &str, x: u16, y: u16| mdc_protocol::InputEvent {
        surface_id: "main".into(),
        pointer_id: 1,
        phase: phase.into(),
        x,
        y,
        node: None,
    };
    let mut tracker = GestureTracker::new(GestureConfig::default());
    assert_eq!(tracker.on_event(&event("down", 10, 10), 0), None);
    assert_eq!(
        tracker.on_event(&event("up", 10, 10), 100),
        Some(Gesture {
            kind: GestureKind::Tap,
            pointer_id: 1,
            x: 10,
            y: 10,
            node: None,
            scale: 1000,
            rotation: 0,
            dx: 0,
            dy: 0,
        })
    );

    // Moving beyond the threshold becomes a drag.
    assert_eq!(tracker.on_event(&event("down", 10, 10), 200), None);
    assert_eq!(
        tracker.on_event(&event("move", 20, 10), 220),
        Some(Gesture {
            kind: GestureKind::Drag,
            pointer_id: 1,
            x: 20,
            y: 10,
            node: None,
            scale: 1000,
            rotation: 0,
            dx: 0,
            dy: 0,
        })
    );
    assert_eq!(
        tracker.on_event(&event("up", 20, 10), 240),
        Some(Gesture {
            kind: GestureKind::Drag,
            pointer_id: 1,
            x: 20,
            y: 10,
            node: None,
            scale: 1000,
            rotation: 0,
            dx: 0,
            dy: 0,
        })
    );

    // Holding past the long-press window emits once.
    assert_eq!(tracker.on_event(&event("down", 5, 5), 1000), None);
    assert_eq!(tracker.tick(1500), None);
    assert_eq!(
        tracker.tick(1600),
        Some(Gesture {
            kind: GestureKind::LongPress,
            pointer_id: 1,
            x: 5,
            y: 5,
            node: None,
            scale: 1000,
            rotation: 0,
            dx: 0,
            dy: 0,
        })
    );
    assert_eq!(tracker.tick(2000), None, "long-press emits once");
    assert_eq!(tracker.on_event(&event("up", 5, 5), 2100), None);
}

#[test]
fn gesture_tracker_recognises_a_two_pointer_pinch() {
    // N725: two pointers derive scale/rotation from their distance/angle;
    // the release is not a tap, and the midpoint is reported.
    let event = |pointer_id: u16, phase: &str, x: u16, y: u16| mdc_protocol::InputEvent {
        surface_id: "main".into(),
        pointer_id,
        phase: phase.into(),
        x,
        y,
        node: None,
    };
    let mut tracker = GestureTracker::new(GestureConfig::default());
    assert_eq!(tracker.on_event(&event(1, "down", 10, 10), 0), None);
    assert_eq!(tracker.on_event(&event(2, "down", 20, 10), 10), None);
    // Distance doubles: 10 -> 20 px, angle unchanged.
    assert_eq!(
        tracker.on_event(&event(2, "move", 30, 10), 20),
        Some(Gesture {
            kind: GestureKind::Pinch,
            pointer_id: 2,
            x: 20,
            y: 10,
            node: None,
            scale: 2000,
            rotation: 0,
            dx: 0,
            dy: 0,
        })
    );
    // Diagonal: distance 10*sqrt(2), angle 45 degrees (scale ~1414).
    let gesture = tracker.on_event(&event(2, "move", 20, 20), 30).unwrap();
    assert_eq!(gesture.kind, GestureKind::Pinch);
    assert_eq!(gesture.scale, 1414);
    assert_eq!(gesture.rotation, 45);
    assert_eq!((gesture.x, gesture.y), (15, 15));
    // Both fingers are consumed by the pinch: no tap on release.
    assert_eq!(tracker.on_event(&event(2, "up", 20, 20), 40), None);
    assert_eq!(tracker.on_event(&event(1, "up", 10, 10), 50), None);
}

#[test]
fn pinch_reaches_the_manager_event_queue() {
    // N725: the Session maps pinch into ManagerEvent::Gesture.
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    let manager = std::sync::Arc::new(std::sync::Mutex::new(DeviceManager::new()));
    session.set_manager(manager.clone());
    session.connect().unwrap();
    handshake(&mut session, &mut device);
    let input = |pointer_id: u16, phase: &str, x: u16, y: u16| mdc_protocol::InputEvent {
        surface_id: "main".into(),
        pointer_id,
        phase: phase.into(),
        x,
        y,
        node: None,
    };
    for (index, event) in [
        input(1, "down", 10, 10),
        input(2, "down", 20, 10),
        input(2, "move", 30, 10),
    ]
    .into_iter()
    .enumerate()
    {
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Input,
                    flags: 0,
                    request_id: index as u32 + 1,
                    payload: encode_control(&event).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
    }
    let events: Vec<ManagerEvent> = manager.lock().unwrap().drain_events().collect();
    let pinch = events
        .iter()
        .find_map(|event| match event {
            ManagerEvent::Gesture { gesture, .. } if gesture.kind == GestureKind::Pinch => {
                Some(gesture)
            }
            _ => None,
        })
        .expect("pinch reaches the manager");
    assert_eq!(pinch.scale, 2000);
    assert_eq!(pinch.rotation, 0);
}

#[test]
fn two_pointer_move_emits_pan_and_pinch_together() {
    // N735: one two-finger move reports both the midpoint translation and
    // the scale/rotation change.
    let event = |pointer_id: u16, phase: &str, x: u16, y: u16| mdc_protocol::InputEvent {
        surface_id: "main".into(),
        pointer_id,
        phase: phase.into(),
        x,
        y,
        node: None,
    };
    let mut tracker = GestureTracker::new(GestureConfig::default());
    assert!(tracker.on_events(&event(1, "down", 10, 10), 0).is_empty());
    assert!(tracker.on_events(&event(2, "down", 20, 10), 10).is_empty());
    let gestures = tracker.on_events(&event(2, "move", 30, 20), 20);
    assert_eq!(gestures.len(), 2, "{gestures:?}");
    assert_eq!(gestures[0].kind, GestureKind::Pan);
    assert_eq!((gestures[0].x, gestures[0].y), (20, 15));
    assert_eq!((gestures[0].dx, gestures[0].dy), (5, 5));
    assert_eq!(gestures[1].kind, GestureKind::Pinch);
    assert_eq!(gestures[1].scale, 2236);
    assert_eq!(gestures[1].rotation, 27);
    // The single-gesture API still reports the richer pinch.
    let mut tracker = GestureTracker::new(GestureConfig::default());
    tracker.on_events(&event(1, "down", 10, 10), 0);
    tracker.on_events(&event(2, "down", 20, 10), 10);
    assert_eq!(
        tracker
            .on_event(&event(2, "move", 30, 20), 20)
            .unwrap()
            .kind,
        GestureKind::Pinch
    );
}

#[test]
fn three_pointer_swipe_reports_direction() {
    // N745: three fingers moving together emit a Swipe when the midpoint
    // travels beyond the threshold; releases do not become taps.
    let event = |pointer_id: u16, phase: &str, x: u16, y: u16| mdc_protocol::InputEvent {
        surface_id: "main".into(),
        pointer_id,
        phase: phase.into(),
        x,
        y,
        node: None,
    };
    let mut tracker = GestureTracker::new(GestureConfig::default());
    for (id, x, y) in [(1, 10, 10), (2, 20, 10), (3, 10, 20)] {
        assert!(tracker.on_events(&event(id, "down", x, y), 0).is_empty());
    }
    let gestures = tracker.on_events(&event(3, "move", 40, 20), 10);
    assert_eq!(gestures.len(), 1);
    assert_eq!(gestures[0].kind, GestureKind::Swipe);
    assert_eq!((gestures[0].dx, gestures[0].dy), (10, 0));
    assert_eq!((gestures[0].x, gestures[0].y), (23, 13));
    assert!(tracker.on_events(&event(3, "up", 40, 20), 20).is_empty());
    assert!(tracker.on_events(&event(2, "up", 20, 10), 30).is_empty());
    assert!(tracker.on_events(&event(1, "up", 10, 10), 40).is_empty());
}

#[test]
fn double_and_triple_taps_share_a_window() {
    // N755: taps inside the window escalate to double/triple and then reset;
    // a tap after the window starts a fresh streak.
    let event = |phase: &str, x: u16, y: u16| mdc_protocol::InputEvent {
        surface_id: "main".into(),
        pointer_id: 1,
        phase: phase.into(),
        x,
        y,
        node: None,
    };
    let tap = |tracker: &mut GestureTracker, at: u64| {
        tracker.on_event(&event("down", 5, 5), at);
        tracker
            .on_event(&event("up", 5, 5), at + 50)
            .map(|g| g.kind)
    };
    let mut tracker = GestureTracker::new(GestureConfig::default());
    assert_eq!(tap(&mut tracker, 0), Some(GestureKind::Tap));
    assert_eq!(tap(&mut tracker, 100), Some(GestureKind::DoubleTap));
    assert_eq!(tap(&mut tracker, 200), Some(GestureKind::TripleTap));
    // The fourth tap starts a new streak, because the triple reset it.
    assert_eq!(tap(&mut tracker, 300), Some(GestureKind::Tap));
    // A slow gap resets to a single tap.
    assert_eq!(tap(&mut tracker, 2_000), Some(GestureKind::Tap));
    // The next tap inside the window is a double again.
    assert_eq!(tap(&mut tracker, 2_200), Some(GestureKind::DoubleTap));
}

#[test]
fn drag_and_long_press_break_the_tap_chain() {
    // N764: only uninterrupted taps escalate; a drag, long press, cancel or
    // multi-touch sequence starts a fresh chain.
    let event = |phase: &str, x: u16, y: u16| mdc_protocol::InputEvent {
        surface_id: "main".into(),
        pointer_id: 1,
        phase: phase.into(),
        x,
        y,
        node: None,
    };
    let mut tracker = GestureTracker::new(GestureConfig::default());
    let tap = |tracker: &mut GestureTracker, at: u64| {
        tracker.on_event(&event("down", 5, 5), at);
        tracker
            .on_event(&event("up", 5, 5), at + 40)
            .map(|g| g.kind)
    };
    assert_eq!(tap(&mut tracker, 0), Some(GestureKind::Tap));
    assert_eq!(tap(&mut tracker, 80), Some(GestureKind::DoubleTap));
    // A drag between taps must not turn the next tap into a triple.
    tracker.on_event(&event("down", 5, 5), 160);
    tracker.on_event(&event("move", 20, 5), 170);
    tracker.on_event(&event("up", 20, 5), 180);
    assert_eq!(tap(&mut tracker, 240), Some(GestureKind::Tap));

    // A long press resets too.
    assert_eq!(tap(&mut tracker, 320), Some(GestureKind::DoubleTap));
    tracker.on_event(&event("down", 5, 5), 400);
    assert!(tracker.tick(1_100).is_some(), "long press emitted");
    tracker.on_event(&event("up", 5, 5), 1_150);
    assert_eq!(tap(&mut tracker, 1_250), Some(GestureKind::Tap));

    // A cancel resets even if the release would have been a tap.
    assert_eq!(tap(&mut tracker, 1_330), Some(GestureKind::DoubleTap));
    tracker.on_event(&event("down", 5, 5), 1_400);
    tracker.on_event(&event("cancel", 5, 5), 1_410);
    assert_eq!(tap(&mut tracker, 1_450), Some(GestureKind::Tap));
}

#[test]
fn multi_tap_requires_the_same_node_and_nearby_position() {
    // N774: time alone is not enough; a distant tap or a tap on another
    // node starts a new chain instead of escalating.
    let event = |phase: &str, x: u16, y: u16, node: Option<&str>| mdc_protocol::InputEvent {
        surface_id: "main".into(),
        pointer_id: 1,
        phase: phase.into(),
        x,
        y,
        node: node.map(str::to_string),
    };
    let tap = |tracker: &mut GestureTracker, at: u64, x: u16, y: u16, node: Option<&str>| {
        tracker.on_event(&event("down", x, y, node), at);
        tracker
            .on_event(&event("up", x, y, node), at + 40)
            .map(|g| g.kind)
    };
    let mut tracker = GestureTracker::new(GestureConfig::default());
    assert_eq!(
        tap(&mut tracker, 0, 10, 10, Some("a")),
        Some(GestureKind::Tap)
    );
    assert_eq!(
        tap(&mut tracker, 80, 12, 11, Some("a")),
        Some(GestureKind::DoubleTap)
    );
    // A different node resets even at the same point.
    assert_eq!(
        tap(&mut tracker, 160, 10, 10, Some("b")),
        Some(GestureKind::Tap)
    );
    // Too far away resets too.
    assert_eq!(
        tap(&mut tracker, 240, 40, 10, Some("b")),
        Some(GestureKind::Tap)
    );
    // Same node and nearby position escalates again.
    assert_eq!(
        tap(&mut tracker, 320, 42, 12, Some("b")),
        Some(GestureKind::DoubleTap)
    );
}

#[test]
fn ota_chunk_ranges_and_missing_ranges() {
    // N737: fixed chunking shortens the final range and subtracts confirmed
    // byte ranges exactly.
    assert_eq!(
        Session::<MemoryLink>::ota_chunk_ranges(10, 4),
        vec![(0, 4), (4, 4), (8, 2)]
    );
    assert_eq!(
        Session::<MemoryLink>::ota_missing_chunks(10, 4, &[(0, 4)]),
        vec![(4, 4), (8, 2)]
    );
    assert_eq!(
        Session::<MemoryLink>::ota_missing_chunks(10, 4, &[(0, 10)]),
        Vec::<(u32, u32)>::new()
    );
}

#[test]
fn ota_chunked_resume_plan_skips_acknowledged_chunks() {
    // N737: at phase 2 the plan contains only missing chunks plus confirm.
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    let mut advertised = caps();
    advertised.ota = true;
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: hello.request_id,
                payload: encode_control(&advertised).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    let plan = session.ota_resume_commands_chunked("2.0.0", 10, 4, &[]);
    assert_eq!(plan.len(), 5, "begin + three chunks + confirm");
    assert_eq!(plan[0].action, "begin");
    session.send_ota(plan[0].clone()).unwrap();
    session.send_ota(plan[1].clone()).unwrap();
    let plan = session.ota_resume_commands_chunked("2.0.0", 10, 4, &[(0, 4)]);
    assert_eq!(plan.len(), 3, "two missing chunks + confirm");
    assert!(plan[..2].iter().all(|command| command.action == "chunk"));
    assert_eq!(plan[0].offset, Some(4));
    assert_eq!(plan[1].offset, Some(8));
    assert_eq!(plan[2].action, "confirm");
}

#[test]
fn ota_phase_checkpoint_resumes_across_reconnect() {
    // N419: the OTA phase survives a reconnect so a driver can resume.
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    // `caps()` disables OTA; advertise it so `send_ota` is allowed.
    let mut advertised = caps();
    advertised.ota = true;
    let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: hello.request_id,
                payload: encode_control(&advertised).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    assert_eq!(session.state, ConnectionState::Ready);
    assert_eq!(session.ota_phase(), 0);

    let command = |action: &str| mdc_protocol::OtaCommand {
        action: action.into(),
        version: None,
        size: None,
        sha256: None,
        offset: None,
        len: None,
    };
    session.send_ota(command("begin")).unwrap();
    assert_eq!(session.ota_phase(), 1);
    session.send_ota(command("chunk")).unwrap();
    assert_eq!(session.ota_phase(), 2);
    // A reconnect edge must not clear the checkpoint.
    session.enable_reconnect(true);
    assert_eq!(session.ota_phase(), 2, "phase survives reconnect");
    // N728: the resume plan only contains the unacknowledged confirm.
    let plan = session.ota_resume_commands("2.0.0", None, None);
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].action, "confirm");
    session.reset_ota();
    assert_eq!(session.ota_phase(), 0);
    assert_eq!(
        session.ota_resume_commands("2.0.0", None, None).len(),
        3,
        "a reset starts from begin"
    );
}

#[test]
fn diagnostics_stay_apart_from_the_frame_path() {
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    let mut boot_caps = caps();
    boot_caps.boot_id = 500;
    boot_caps.run_id = 1;
    write_caps(&mut device, hello.request_id, &boot_caps);
    session.poll().unwrap();
    let _request = session.send_frame(frame(&[3; 8])).unwrap();
    assert_eq!(session.pending_request_count(), 1);

    let metric = MetricReport {
        name: "heap".into(),
        value: 1024.0,
        unit: Some("bytes".into()),
        boot_id: 0,
        run_id: 0,
        event_seq: 1,
    };
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Metric,
                flags: 0,
                request_id: 0,
                payload: encode_control(&metric).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    assert_eq!(session.diagnostic_count(), 1);
    assert_eq!(
        session.pending_request_count(),
        1,
        "a metric must not clear a frame ACK"
    );
    assert_eq!(
        session.take_diagnostics(),
        vec![DiagnosticRecord::Metric(metric)]
    );
    assert_eq!(session.diagnostic_count(), 0);

    // The channel is bounded: the 65th record drops the oldest.
    for seq in 0..(MAX_DIAGNOSTICS as u64 + 5) {
        let metric = MetricReport {
            name: "seq".into(),
            value: seq as f64,
            unit: None,
            boot_id: 0,
            run_id: 0,
            event_seq: seq,
        };
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Metric,
                    flags: 0,
                    request_id: 0,
                    payload: encode_control(&metric).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
    }
    assert_eq!(session.diagnostic_count(), MAX_DIAGNOSTICS);
    let records = session.take_diagnostics();
    assert_eq!(
        records.first(),
        Some(&DiagnosticRecord::Metric(MetricReport {
            name: "seq".into(),
            value: 5.0,
            unit: None,
            boot_id: 0,
            run_id: 0,
            event_seq: 5,
        }))
    );

    // A BootInfo (N316) with a new boot id discards the old run.
    let boot = BootInfo {
        firmware: "2.0.0".into(),
        boot_id: 999,
        run_id: 7,
        reset_reason: Some("watchdog".into()),
    };
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::BootInfo,
                flags: 0,
                request_id: 0,
                payload: encode_control(&boot).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    assert_eq!(session.device_boot_id(), Some(999));
    assert_eq!(session.pending_request_count(), 0);
    assert!(session.needs_full_frame);
    let records = session.take_diagnostics();
    assert!(matches!(
        records.as_slice(),
        [DiagnosticRecord::BootInfo(_)]
    ));
}

#[test]
fn telemetry_also_emits_a_manager_event() {
    let (host, mut device) = MemoryLink::pair();
    let manager = Arc::new(Mutex::new(DeviceManager::new()));
    let mut session = Session::new(host);
    session.set_manager(manager.clone());
    session.connect().unwrap();
    let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    write_caps(&mut device, hello.request_id, &caps());
    session.poll().unwrap();
    let event = mdc_protocol::TelemetryEvent {
        name: "soak".into(),
        level: Some("info".into()),
        fields: std::collections::BTreeMap::new(),
        boot_id: 0,
        run_id: 0,
        event_seq: 1,
    };
    device
        .write(
            &Packet {
                version: VERSION,
                kind: MessageType::Telemetry,
                flags: 0,
                request_id: 0,
                payload: encode_control(&event).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
    session.poll().unwrap();
    // The full record is in the diagnostics queue ...
    assert!(matches!(
        session.take_diagnostics().as_slice(),
        [DiagnosticRecord::Telemetry(_)]
    ));
    // ... and a metadata-only event is on the manager queue (N357).
    let events: Vec<ManagerEvent> = manager.lock().unwrap().drain_events().collect();
    assert!(events.iter().any(|event| matches!(
        event,
        ManagerEvent::Telemetry { name, level, .. }
            if name == "soak" && level.as_deref() == Some("info")
    )));
}

#[test]
fn frame_pacing_clamps_fps_and_can_be_cleared() {
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    handshake(&mut session, &mut device);
    session.set_frame_pacing(8, 0); // 125 ms interval, unlimited bytes
                                    // First frame at virtual 0 is sent.
    assert!(session.send_frame(frame(&[1; 8])).is_ok());
    // 10 ms later it is too soon.
    session.tick(10).unwrap();
    assert_eq!(
        session.send_frame(frame(&[2; 8])),
        Err(CoreError::Throttled)
    );
    assert_eq!(
        session.pacing_dropped(),
        0,
        "the fps gate is not a budget drop"
    );
    // At the interval the next frame passes.
    session.tick(125).unwrap();
    assert!(session.send_frame(frame(&[3; 8])).is_ok());
    // Clearing the pacing removes all gating.
    session.clear_frame_pacing();
    session.tick(126).unwrap();
    assert!(session.send_frame(frame(&[4; 8])).is_ok());
}

#[test]
fn frame_pacing_drops_a_frame_over_the_byte_budget() {
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    // A 40x40 surface so one frame spans several 1 KiB tiles.
    let mut custom = caps();
    custom.surfaces[0] = Surface {
        width: 40,
        height: 40,
        ..s()
    };
    let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
        .unwrap()
        .0;
    write_caps(&mut device, hello.request_id, &custom);
    session.poll().unwrap();
    // 8 FPS -> 125 ms -> 8 B/s budget = 1 byte per interval, far below a frame.
    session.set_frame_pacing(8, 8);
    let big = Frame {
        surface_id: "main".into(),
        width: 40,
        height: 40,
        bytes: vec![0; 40 * 40 * 2],
    };
    assert_eq!(session.send_frame(big), Err(CoreError::Throttled));
    assert_eq!(session.pacing_dropped(), 1);
}

#[test]
fn preferred_tile_size_drives_the_host_cut() {
    // No advertised preference -> the host's default edge (32).
    let mut c = caps();
    c.surfaces[0] = Surface {
        width: 70,
        height: 40,
        ..s()
    };
    let device = Device::new(c);
    assert_eq!(
        device.plan_surface_tiles("main"),
        vec![
            (0, 0, 32, 32),
            (32, 0, 32, 32),
            (64, 0, 6, 32),
            (0, 32, 32, 8),
            (32, 32, 32, 8),
            (64, 32, 6, 8),
        ]
    );
    // The device's preferred tile size is used when present.
    let mut c2 = caps();
    c2.surfaces[0] = Surface {
        width: 40,
        height: 40,
        preferred_tile_size: 20,
        ..s()
    };
    let device2 = Device::new(c2);
    assert_eq!(
        device2.plan_surface_tiles("main"),
        vec![
            (0, 0, 20, 20),
            (20, 0, 20, 20),
            (0, 20, 20, 20),
            (20, 20, 20, 20)
        ]
    );
    // Degenerate surfaces never produce tiles.
    assert!(plan_tiles(0, 10, 32).is_empty());
    assert!(plan_tiles(10, 0, 32).is_empty());
    assert!(device.plan_surface_tiles("missing").is_empty());
}

#[test]
fn unimplemented_pixel_format_is_rejected_explicitly() {
    let mut c = caps();
    c.surfaces[0].pixel_format = mdc_protocol::PixelFormat::Rgba8888;
    let device = Device::new(c);
    let error = device.validate_frame(&frame(&[0; 8])).unwrap_err();
    assert_eq!(error, CoreError::Format("RGBA8888".into()));
}

struct BurstLink {
    bytes: Vec<u8>,
    read_calls: usize,
}
impl mdc_transport::Transport for BurstLink {
    fn write(&mut self, _bytes: &[u8]) -> Result<(), mdc_transport::TransportError> {
        Ok(())
    }
    fn read(&mut self) -> Result<Option<Vec<u8>>, mdc_transport::TransportError> {
        if self.bytes.is_empty() {
            return Ok(None);
        }
        self.read_calls += 1;
        Ok(Some(std::mem::take(&mut self.bytes)))
    }
    fn close(&mut self) {}
}

struct FloodLink {
    chunk: Vec<u8>,
    calls: usize,
}
impl mdc_transport::Transport for FloodLink {
    fn write(&mut self, _bytes: &[u8]) -> Result<(), mdc_transport::TransportError> {
        Ok(())
    }
    fn read(&mut self) -> Result<Option<Vec<u8>>, mdc_transport::TransportError> {
        self.calls += 1;
        Ok(Some(self.chunk.clone()))
    }
    fn close(&mut self) {}
}

fn pong_packets(count: u32, payload_len: usize) -> (Vec<u8>, usize) {
    let mut wire = Vec::new();
    for id in 0..count {
        wire.extend(
            Packet {
                version: VERSION,
                kind: MessageType::Pong,
                flags: 0,
                request_id: id + 1,
                payload: vec![0; payload_len],
            }
            .encode(1 << 20)
            .unwrap(),
        );
    }
    let packet_len = 20 + payload_len;
    (wire, packet_len)
}

#[test]
fn poll_handles_at_most_32_packets() {
    // SIM-R07/SIM-T08: a burst is parsed over several polls.
    let (wire, packet_len) = pong_packets(100, 0);
    let total = wire.len();
    let mut session = Session::new(BurstLink {
        bytes: wire,
        read_calls: 0,
    });
    session.poll().unwrap();
    assert_eq!(session.transport.read_calls, 1);
    assert_eq!(total - session.rx.len(), MAX_POLL_PACKETS * packet_len);
    assert!(!session.rx.is_empty());
}

#[test]
fn poll_handles_at_most_64_kib_per_call() {
    // SIM-R07: large packets hit the byte budget before the packet budget.
    let (wire, packet_len) = pong_packets(40, 3000);
    let total = wire.len();
    let mut session = Session::new(BurstLink {
        bytes: wire,
        read_calls: 0,
    });
    session.poll().unwrap();
    let consumed = total - session.rx.len();
    assert!(consumed >= MAX_POLL_BYTES, "consumed {consumed}");
    assert!(
        consumed < MAX_POLL_BYTES + packet_len,
        "consumed {consumed}"
    );
    assert!(session.transport.read_calls <= 1);
}

#[test]
fn poll_stops_reading_after_the_read_budget() {
    // SIM-R07: a peer that always has more noise cannot keep one poll busy.
    let mut session = Session::new(FloodLink {
        chunk: vec![0u8; 1024],
        calls: 0,
    });
    session.poll().unwrap();
    assert_eq!(session.transport.calls, MAX_POLL_BYTES / 1024);
    assert!(session.rx.len() <= 1);
}

#[test]
fn finished_ack_history_is_bounded() {
    // SIM-R07/SIM-T13: 100000 successful ACKs do not grow history.
    let (host, _device) = MemoryLink::pair();
    let mut session = Session::new(host);
    for id in 0..100_000u32 {
        session.record_finished_ack(id);
    }
    assert_eq!(session.finished_acks.len(), MAX_FINISHED_ACKS);
    assert!(!session.finished_acks.contains(&0));
    assert!(session.finished_acks.contains(&99_999));
}

#[test]
fn late_ack_after_history_eviction_is_harmless() {
    // SIM-R07: once an ACK leaves the bounded window, a replay must not
    // relaunch a frame because the request was already released.
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect().unwrap();
    handshake(&mut session, &mut device);
    let request = session.send_frame(frame(&[1; 8])).unwrap();
    while device.read().unwrap().is_some() {}
    let ack = |request: u32| Packet {
        version: VERSION,
        kind: MessageType::Ack,
        flags: 0,
        request_id: request,
        payload: encode_control(&Ack {
            received: true,
            displayed: true,
            frame_id: request as u64,
            error: None,
            run_id: 0,
            event_seq: 0,
        })
        .unwrap(),
    };
    device.write(&ack(request).encode(4096).unwrap()).unwrap();
    session.poll().unwrap();
    while device.read().unwrap().is_some() {}
    for id in 0..(MAX_FINISHED_ACKS as u32 + 10) {
        session.record_finished_ack(id + 100_000);
    }
    assert!(!session.finished_acks.contains(&request));
    device.write(&ack(request).encode(4096).unwrap()).unwrap();
    session.poll().unwrap();
    assert!(device.read().unwrap().is_none());
}

#[test]
fn displayed_timing_history_is_bounded() {
    let (host, _device) = MemoryLink::pair();
    let mut session = Session::new(host);
    for id in 0..(MAX_DISPLAY_TIMINGS as u32 + 50) {
        session.frame_sent_at.insert(id, std::time::Instant::now());
        session.note_displayed(id);
    }
    assert_eq!(session.displayed_timings.len(), MAX_DISPLAY_TIMINGS);
}

#[test]
fn active_pointer_map_is_bounded() {
    // SIM-R07: missing "up" events cannot allocate an unbounded map.
    let event = |id: u16| mdc_protocol::InputEvent {
        surface_id: "main".into(),
        pointer_id: id,
        phase: "down".into(),
        x: 1,
        y: 1,
        node: None,
    };
    let mut tracker = GestureTracker::default();
    for id in 0..100u16 {
        tracker.on_event(&event(id), 0);
    }
    assert_eq!(tracker.pointers.len(), MAX_ACTIVE_POINTERS);
}

#[test]
fn frame_assembler_rejects_too_many_ranges() {
    // SIM-R07: alternating one-byte chunks cannot grow the range list without bound.
    let mut tx = FrameAssembler::begin(1, 1024, 4).unwrap();
    for index in 0..MAX_FRAME_RANGES {
        tx.chunk(index * 2, &[1]).unwrap();
    }
    assert_eq!(
        tx.chunk(MAX_FRAME_RANGES * 2, &[1]),
        Err(TransactionNack::TooManyRanges)
    );
}
