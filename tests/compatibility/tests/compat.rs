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
            config: mdc_protocol::ConfigCapability::default(),
            scene: mdc_protocol::SceneCapability::default(),
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
    let mesh =
        mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::full_3d()).unwrap();
    assert!(mesh
        .accepted_by(&mdc_model::RendererCapability::full_3d())
        .is_ok());
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
        .feed(&packet(mdc_protocol::encode_frame_chunk(
            2,
            2,
            &[0x1F, 0x00],
        )))
        .unwrap();
    receiver
        .feed(&packet(mdc_protocol::encode_frame_commit(2)))
        .unwrap();
    let stats = receiver.stats();
    assert_eq!(stats.frames_published, 1);
    assert!(stats.frames_dropped >= 1);
}

// ---- N47: configuration fixtures ----

#[test]
fn config_rejects_unknown_key_oversize_and_device_invalid_value() {
    use mdc_core::{ConnectionState, CoreError, Session};
    let (host, mut device) = FakeDevice::pair(BoardProfile::default());
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

    // Unadvertised key is refused host-side.
    assert!(matches!(
        session.set_config("cpu", "1"),
        Err(CoreError::Unsupported)
    ));

    // A value over the host limit is refused before any packet.
    let big = "x".repeat(mdc_core::MAX_CONFIG_VALUE_BYTES + 1);
    assert!(matches!(
        session.set_config("brightness", &big),
        Err(CoreError::TooLarge)
    ));

    // A valid set is accepted and cached.
    session.set_config("brightness", "200").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(session.config("brightness"), Some("200"));

    // An out-of-range value is rejected by the device; the previous value stays.
    session.set_config("brightness", "999").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(session.config("brightness"), Some("200"));
}

// ---- N65: baked package + scene fixtures ----

fn baked_package(frames: u32, pixels: usize) -> mdc_model::MdcPackage {
    let atlas = mdc_model::AtlasBlob {
        sprite_width: 2,
        sprite_height: 2,
        frames,
        columns: frames.max(1),
        rows: 1,
        rgb565: vec![0u8; pixels],
        clips: Vec::new(),
    };
    let package = mdc_model::DevicePackage {
        target: "esp32-plane".into(),
        mode: mdc_model::PackageMode::BakedSprites,
        nodes: Vec::new(),
        meshes: Vec::new(),
        textures: Vec::new(),
        materials: Vec::new(),
        skeletons: Vec::new(),
        animations: Vec::new(),
        sprites: Vec::new(),
        layers: Vec::new(),
        estimated_bytes: pixels as u64,
        atlas: Some(atlas),
        texture: None,
    };
    mdc_model::MdcPackage::seal(package).unwrap()
}

#[test]
fn baked_package_with_a_wrong_pixel_size_is_rejected() {
    // 2 frames of 2x2 RGB565 need 16 bytes, not 4.
    let package = baked_package(2, 4);
    assert!(matches!(
        package.accepted_by(&mdc_model::RendererCapability::sprite_only()),
        Err(mdc_model::ModelError::Unsupported(_))
    ));
    let good = baked_package(2, 16);
    assert!(good
        .accepted_by(&mdc_model::RendererCapability::sprite_only())
        .is_ok());
}

#[test]
fn baked_package_needs_sprite_support() {
    let package = baked_package(2, 16);
    let no_sprite = mdc_model::RendererCapability {
        sprite: false,
        ..mdc_model::RendererCapability::sprite_only()
    };
    assert!(matches!(
        package.accepted_by(&no_sprite),
        Err(mdc_model::ModelError::Unsupported(_))
    ));
}

#[test]
fn scene_over_the_depth_limit_is_refused() {
    use mdc_core::{ConnectionState, Session};
    use mdc_protocol::SceneNode;

    let (host, mut device) = FakeDevice::pair(BoardProfile::default());
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

    // Default board advertises max_depth = 4; build a chain of 6.
    let mut node = SceneNode {
        name: "leaf".into(),
        kind: "rect".into(),
        rect: None,
        children: Vec::new(),
        value: None,
    };
    for i in 0..5 {
        node = SceneNode {
            name: format!("n{i}"),
            kind: "rect".into(),
            rect: None,
            children: vec![node],
            value: None,
        };
    }
    assert!(matches!(
        session.set_scene(node),
        Err(mdc_core::CoreError::TooLarge)
    ));
}

// ---- N76: animation fixtures ----

#[test]
fn animation_requires_a_baked_atlas_and_a_known_clip() {
    use mdc_core::{ConnectionState, Session};

    let (host, mut device) = FakeDevice::pair(BoardProfile::default());
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

    // No atlas installed yet: the device refuses the animation setting.
    session.set_config("animation", "idle").unwrap();
    device.poll().unwrap();
    let _ = session.poll(); // the refused reply surfaces as a protocol error
    assert_eq!(session.active_animation(), None);

    // Install an atlas with one "idle" clip.
    let atlas = mdc_model::AtlasBlob {
        sprite_width: 2,
        sprite_height: 2,
        frames: 2,
        columns: 2,
        rows: 1,
        rgb565: vec![0u8; 16],
        clips: vec![mdc_model::AtlasClip {
            name: "idle".into(),
            start: 0,
            frames: 2,
            fps: 30,
            looping: true,
            fade_in_ms: 0,
            fade_out_ms: 0,
        }],
    };
    let package = mdc_model::DevicePackage {
        target: "esp32-plane".into(),
        mode: mdc_model::PackageMode::BakedSprites,
        nodes: Vec::new(),
        meshes: Vec::new(),
        textures: Vec::new(),
        materials: Vec::new(),
        skeletons: Vec::new(),
        animations: Vec::new(),
        sprites: Vec::new(),
        layers: Vec::new(),
        estimated_bytes: 16,
        atlas: Some(atlas),
        texture: None,
    };
    let sealed = mdc_model::MdcPackage::seal(package).unwrap();
    session
        .install_package(&sealed.to_bytes().unwrap())
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();

    // An unknown clip is refused and leaves nothing active.
    session.set_config("animation", "walk").unwrap();
    device.poll().unwrap();
    let _ = session.poll();
    assert_eq!(session.active_animation(), None);

    session.set_config("animation", "idle").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(session.active_animation(), Some("idle"));
}

// ---- N84: scene geometry fixtures ----

#[test]
fn non_rect_scene_kinds_are_recorded_but_not_rasterized() {
    use mdc_core::{ConnectionState, Session};
    use mdc_protocol::SceneNode;

    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 4,
        height: 4,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

    session
        .set_scene(SceneNode {
            name: "label".into(),
            kind: "custom".into(),
            rect: Some([0, 0, 4, 4]),
            children: Vec::new(),
            value: Some("hi".into()),
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.has_scene());
    assert!(!device.render_scene(), "an unknown kind is not rasterized");
    assert_eq!(
        device.pixel_rgb565(0, 0),
        Some(0x0000),
        "framebuffer untouched"
    );
}

#[test]
fn out_of_bounds_rects_are_clipped_without_panicking() {
    use mdc_core::{ConnectionState, Session};
    use mdc_protocol::SceneNode;

    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 4,
        height: 4,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

    session
        .set_scene(SceneNode {
            name: "bg".into(),
            kind: "rect".into(),
            rect: Some([2, 2, 100, 100]),
            children: Vec::new(),
            value: Some("#FF0000".into()),
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_scene());
    assert_eq!(
        device.pixel_rgb565(2, 2),
        Some(0xF800),
        "in-bounds part drawn"
    );
    assert_eq!(
        device.pixel_rgb565(0, 0),
        Some(0x0000),
        "outside stays clear"
    );
}

#[test]
fn textured_skinned_bake_reaches_a_sim_device() {
    use mdc_core::{ConnectionState, Session};
    use mdc_model::{Animation, AnimationTrack, Mesh, Model, Node, Skeleton};
    use mdc_simulator::{BoardProfile, FakeDevice};

    let mesh = Mesh {
        name: "tri".into(),
        vertices: 3,
        indices: 0,
        material: None,
        preview_positions: vec![[0.0, 0.0, 0.0], [0.5, 0.0, 0.0], [0.0, 0.5, 0.0]],
        skin_joints: vec![[1, 0, 0, 0], [1, 0, 0, 0], [1, 0, 0, 0]],
        skin_weights: vec![
            [1.0, 0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0, 0.0],
        ],
        preview_uv: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
        ..Mesh::default()
    };
    let identity = [
        1.0f32, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ];
    let skeleton = Skeleton {
        name: "rig".into(),
        bones: 2,
        joint_nodes: vec![0, 1],
        inverse_bind: vec![identity, identity],
    };
    let quarter = std::f32::consts::FRAC_1_SQRT_2;
    let animation = Animation {
        name: "spin".into(),
        channels: 1,
        tracks: vec![AnimationTrack {
            node: 1,
            path: "rotation".into(),
            interpolation: "LINEAR".into(),
            times: vec![0.0, 1.0],
            values: vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, quarter, quarter],
        }],
    };
    let model = Model {
        source: "test".into(),
        nodes: vec![
            Node {
                name: "root".into(),
                children: vec![1],
                ..Node::default()
            },
            Node {
                name: "j1".into(),
                mesh: Some(0),
                ..Node::default()
            },
        ],
        meshes: vec![mesh],
        skeletons: vec![skeleton],
        animations: vec![animation],
        ..Model::default()
    };
    let texture = mdc_bake::TextureMap::from_rgb565(2, 1, vec![0x00, 0xF8, 0xE0, 0x07]).unwrap();
    let package =
        mdc_bake::bake_textured_to_package(&model, 0, 3, (8, 8), Some(&texture), 0x001F).unwrap();
    assert!(package
        .accepted_by(&mdc_model::RendererCapability::sprite_only())
        .is_ok());

    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 8,
        height: 8,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    session
        .install_package(&package.to_bytes().unwrap())
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(
        device.package_ready(),
        "device accepted the textured package"
    );
}

#[test]
fn textured_bake_refuses_an_over_budget_texture() {
    // The texture map and the bake both enforce a size budget; a mismatched
    // buffer is rejected rather than silently truncated.
    assert!(matches!(
        mdc_bake::TextureMap::from_rgb565(4, 4, vec![0u8; 8]),
        Err(mdc_bake::BakeError::Budget(_, _))
    ));
}

#[test]
fn full_geometry_reaches_the_device_and_renders() {
    use mdc_core::{ConnectionState, Session};
    use mdc_model::{MeshGeometry, Node};
    use mdc_simulator::{BoardProfile, FakeDevice};

    let geometry = MeshGeometry {
        positions: vec![
            [-0.5, -0.5, 0.0],
            [0.5, -0.5, 0.0],
            [0.5, 0.5, 0.0],
            [-0.5, 0.5, 0.0],
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
        uv: Vec::new(),
        skin_joints: Vec::new(),
        skin_weights: Vec::new(),
        morph_targets: Vec::new(),
    };
    let mesh = mdc_model::Mesh {
        name: "quad".into(),
        vertices: 4,
        indices: 6,
        material: None,
        geometry: Some(geometry),
        ..mdc_model::Mesh::default()
    };
    let package = mdc_model::DevicePackage {
        target: "linux-mesh".into(),
        mode: mdc_model::PackageMode::Mesh,
        nodes: vec![Node {
            name: "root".into(),
            mesh: Some(0),
            ..Node::default()
        }],
        meshes: vec![mesh],
        textures: Vec::new(),
        materials: Vec::new(),
        skeletons: Vec::new(),
        animations: Vec::new(),
        sprites: Vec::new(),
        layers: Vec::new(),
        estimated_bytes: 48,
        atlas: None,
        texture: None,
    };
    let sealed = mdc_model::MdcPackage::seal(package).unwrap();

    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 16,
        height: 16,
        renderer_mesh2d: true,
        renderer_max_vertices: 4096,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    session
        .install_package(&sealed.to_bytes().unwrap())
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.package_ready());
    assert!(device.render_preview());
    let covered = (0u16..16)
        .flat_map(|y| (0u16..16).map(move |x| (x, y)))
        .filter(|(x, y)| device.pixel_rgb565(*x, *y).unwrap_or(0) != 0)
        .count();
    assert!(covered > 40, "device drew the full quad ({covered} px)");
}

#[test]
fn live2d_motion_curve_sampling_is_deterministic() {
    use mdc_model::sample_segments;
    // Malformed / empty streams never panic and never allocate unboundedly.
    assert_eq!(sample_segments(&[], 0.5), 0.0);
    assert_eq!(sample_segments(&[3.0], 0.5), 3.0);
    // A truncated bezier falls back to the running value.
    let truncated = [0.0, 1.0, 0.25, 0.0];
    assert_eq!(sample_segments(&truncated, 0.5), 0.0);
    // Two linear segments chain: 0 -> 10 over 1s, then hold 10.
    let chained = [0.0, 0.0, 0.5, 10.0, 0.0, 1.0, 10.0];
    assert_eq!(sample_segments(&chained, 0.25), 5.0);
    assert_eq!(sample_segments(&chained, 0.75), 10.0);
    assert_eq!(sample_segments(&chained, 5.0), 10.0);
}

#[test]
fn material_colour_tints_the_package_texture_on_the_device() {
    use mdc_core::{ConnectionState, Session};
    use mdc_model::{DevicePackage, Material, Mesh, MeshGeometry, Node, PackageMode, TextureBlob};
    use mdc_simulator::{BoardProfile, FakeDevice};

    let mesh = Mesh {
        name: "quad".into(),
        vertices: 4,
        indices: 6,
        material: Some(0),
        geometry: Some(MeshGeometry {
            positions: vec![
                [-0.5, -0.5, 0.0],
                [0.5, -0.5, 0.0],
                [0.5, 0.5, 0.0],
                [-0.5, 0.5, 0.0],
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            uv: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            skin_joints: Vec::new(),
            skin_weights: Vec::new(),
            morph_targets: Vec::new(),
        }),
        ..Mesh::default()
    };
    let package = DevicePackage {
        target: "linux-mesh".into(),
        mode: PackageMode::Mesh,
        nodes: vec![Node {
            name: "root".into(),
            mesh: Some(0),
            ..Node::default()
        }],
        meshes: vec![mesh],
        textures: Vec::new(),
        materials: vec![Material {
            name: "skin".into(),
            texture: None,
            // Half-intensity red: the texture is a full red, so the result halves.
            color: Some([128, 0, 0]),
        }],
        skeletons: Vec::new(),
        animations: Vec::new(),
        sprites: Vec::new(),
        layers: Vec::new(),
        estimated_bytes: 48,
        atlas: None,
        texture: Some(TextureBlob {
            width: 1,
            height: 1,
            rgb565: vec![0x00, 0xF8], // 1×1 red
        }),
    };
    let sealed = mdc_model::MdcPackage::seal(package).unwrap();

    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 16,
        height: 16,
        renderer_mesh2d: true,
        renderer_max_vertices: 4096,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    session
        .install_package(&sealed.to_bytes().unwrap())
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_preview());
    let centre = device.pixel_rgb565(8, 8).unwrap();
    // Tinted: red ~128 -> the red channel is roughly half, not full.
    let r = (centre >> 11) & 0x1f;
    assert!((10..=22).contains(&r), "tinted red channel {r}");
}

#[test]
fn cubicspline_sampling_is_deterministic_and_bounded() {
    use mdc_model::AnimationTrack;
    let cubic = AnimationTrack {
        node: 0,
        path: "translation".into(),
        interpolation: "CUBICSPLINE".into(),
        times: vec![0.0, 1.0],
        values: vec![0.0, 0.0, 0.0, 0.0, 10.0, 0.0],
    };
    let linear = AnimationTrack {
        node: 0,
        path: "translation".into(),
        interpolation: "LINEAR".into(),
        times: vec![0.0, 1.0],
        values: vec![0.0, 10.0],
    };
    // Cubic eases in, so its quarter point differs from the linear one.
    let cubic_quarter = mdc_bake::sample_channel(&cubic, 1, 0.25)[0];
    let linear_quarter = mdc_bake::sample_channel(&linear, 1, 0.25)[0];
    assert!(cubic_quarter < linear_quarter);
    // A truncated cubic stream never panics and stays finite.
    let truncated = AnimationTrack {
        values: vec![0.0, 1.0],
        ..cubic
    };
    let value = mdc_bake::sample_channel(&truncated, 1, 0.5)[0];
    assert!(value.is_finite());
    // Looping wraps negative time too.
    assert_eq!(
        mdc_bake::sample_channel_looped(&linear, 1, -0.5, 0.0, 1.0, true),
        vec![5.0]
    );
}

#[test]
fn device_renders_a_morph_animated_geometry_package() {
    use mdc_core::{ConnectionState, Session};
    use mdc_model::{Animation, AnimationTrack, Material, Mesh, MeshGeometry, Model, Node};
    use mdc_simulator::{BoardProfile, FakeDevice};

    let model = Model {
        source: "test".into(),
        nodes: vec![Node {
            name: "face".into(),
            mesh: Some(0),
            ..Node::default()
        }],
        meshes: vec![Mesh {
            name: "face".into(),
            vertices: 4,
            indices: 6,
            material: Some(0),
            geometry: Some(MeshGeometry {
                positions: vec![
                    [-0.5, -0.5, 0.0],
                    [0.5, -0.5, 0.0],
                    [0.5, 0.5, 0.0],
                    [-0.5, 0.5, 0.0],
                ],
                indices: vec![0, 1, 2, 0, 2, 3],
                uv: Vec::new(),
                skin_joints: Vec::new(),
                skin_weights: Vec::new(),
                morph_targets: vec![vec![
                    [0.6, 0.0, 0.0],
                    [0.6, 0.0, 0.0],
                    [0.6, 0.0, 0.0],
                    [0.6, 0.0, 0.0],
                ]],
            }),
            morph_weights: vec![0.0],
            ..Mesh::default()
        }],
        materials: vec![Material {
            name: "red".into(),
            texture: None,
            color: Some([255, 0, 0]),
        }],
        animations: vec![Animation {
            name: "smile".into(),
            channels: 1,
            tracks: vec![AnimationTrack {
                node: 0,
                path: "weights".into(),
                interpolation: "LINEAR".into(),
                times: vec![0.0, 1.0],
                values: vec![0.0, 1.0],
            }],
        }],
        ..Model::default()
    };
    let package = mdc_bake::mesh_package_with_texture(
        &model,
        &mdc_model::RendererCapability::full_3d(),
        None,
    )
    .unwrap();

    let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    session
        .install_package(&package.to_bytes().unwrap())
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    session.start_mirror("main", "full-frame", 30).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();

    device.clock.advance(0);
    device.emit_mirror_frame().unwrap();
    session.poll().unwrap();
    let neutral = session.take_mirror_frame().expect("first frame").bytes;
    device.clock.advance(1000);
    device.emit_mirror_frame().unwrap();
    session.poll().unwrap();
    let smiled = session.take_mirror_frame().expect("second frame").bytes;
    assert_ne!(
        neutral, smiled,
        "the geometry morph changed the mirrored frame"
    );
}

#[test]
fn device_renders_an_animated_mesh_package_scene() {
    use mdc_core::{ConnectionState, Session};
    use mdc_model::{Animation, AnimationTrack, Material, Mesh, MeshGeometry, Model, Node};
    use mdc_simulator::{BoardProfile, FakeDevice};

    let quad = |name: &str, material: usize| Mesh {
        name: name.into(),
        vertices: 4,
        indices: 6,
        material: Some(material),
        geometry: Some(MeshGeometry {
            positions: vec![
                [-0.5, -0.5, 0.0],
                [0.5, -0.5, 0.0],
                [0.5, 0.5, 0.0],
                [-0.5, 0.5, 0.0],
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            uv: Vec::new(),
            skin_joints: Vec::new(),
            skin_weights: Vec::new(),
            morph_targets: Vec::new(),
        }),
        ..Mesh::default()
    };
    let model = Model {
        source: "test".into(),
        nodes: vec![
            Node {
                name: "root".into(),
                children: vec![1],
                ..Node::default()
            },
            Node {
                name: "body".into(),
                mesh: Some(0),
                translation: [-0.6, 0.0, 0.0],
                ..Node::default()
            },
        ],
        meshes: vec![quad("body", 0)],
        materials: vec![Material {
            name: "red".into(),
            texture: None,
            color: Some([255, 0, 0]),
        }],
        animations: vec![Animation {
            name: "slide".into(),
            channels: 1,
            tracks: vec![AnimationTrack {
                node: 1,
                path: "translation".into(),
                interpolation: "LINEAR".into(),
                times: vec![0.0, 1.0],
                values: vec![-0.6, 0.0, 0.0, 0.6, 0.0, 0.0],
            }],
        }],
        ..Model::default()
    };
    let package = mdc_bake::mesh_package_with_texture(
        &model,
        &mdc_model::RendererCapability::full_3d(),
        None,
    )
    .unwrap();

    let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    session
        .install_package(&package.to_bytes().unwrap())
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.package_ready());
    session.start_mirror("main", "full-frame", 30).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();

    device.clock.advance(0);
    device.emit_mirror_frame().unwrap();
    session.poll().unwrap();
    let first = session
        .take_mirror_frame()
        .expect("first mirror frame")
        .bytes;
    device.clock.advance(1000);
    device.emit_mirror_frame().unwrap();
    session.poll().unwrap();
    let second = session
        .take_mirror_frame()
        .expect("second mirror frame")
        .bytes;
    assert_ne!(
        first, second,
        "the node animation moved the mesh between frames"
    );
    assert!(first.iter().any(|b| *b != 0), "the device drew something");
}

#[test]
fn multi_mesh_scene_bakes_and_reaches_a_device() {
    use mdc_core::{ConnectionState, Session};
    use mdc_model::{Material, Mesh, MeshGeometry, Model, Node};
    use mdc_simulator::{BoardProfile, FakeDevice};

    let quad = |name: &str, material: usize| Mesh {
        name: name.into(),
        vertices: 4,
        indices: 6,
        material: Some(material),
        geometry: Some(MeshGeometry {
            positions: vec![
                [-0.5, -0.5, 0.0],
                [0.5, -0.5, 0.0],
                [0.5, 0.5, 0.0],
                [-0.5, 0.5, 0.0],
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            uv: Vec::new(),
            skin_joints: Vec::new(),
            skin_weights: Vec::new(),
            morph_targets: Vec::new(),
        }),
        ..Mesh::default()
    };
    let model = Model {
        source: "test".into(),
        nodes: vec![
            Node {
                name: "root".into(),
                children: vec![1, 2],
                ..Node::default()
            },
            Node {
                name: "left".into(),
                mesh: Some(0),
                translation: [-0.6, 0.0, 0.0],
                ..Node::default()
            },
            Node {
                name: "right".into(),
                mesh: Some(1),
                translation: [0.6, 0.0, 0.0],
                ..Node::default()
            },
        ],
        meshes: vec![quad("left", 0), quad("right", 1)],
        materials: vec![
            Material {
                name: "red".into(),
                texture: None,
                color: Some([255, 0, 0]),
            },
            Material {
                name: "green".into(),
                texture: None,
                color: Some([0, 255, 0]),
            },
        ],
        ..Model::default()
    };
    let package = mdc_bake::bake_scene_to_package(&model, 0, 3, (32, 16), None, 0xFFFF).unwrap();
    assert!(package
        .accepted_by(&mdc_model::RendererCapability::sprite_only())
        .is_ok());

    let (host, mut device) = FakeDevice::pair(BoardProfile::default());
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    session
        .install_package(&package.to_bytes().unwrap())
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.package_ready());
    assert_eq!(device.atlas_frames(), 3);
}

#[test]
fn textured_geometry_package_renders_on_the_device() {
    use mdc_core::{ConnectionState, Session};
    use mdc_model::{DevicePackage, Material, Mesh, MeshGeometry, Node, PackageMode, TextureBlob};
    use mdc_simulator::{BoardProfile, FakeDevice};

    let mesh = Mesh {
        name: "quad".into(),
        vertices: 4,
        indices: 6,
        material: Some(0),
        geometry: Some(MeshGeometry {
            positions: vec![
                [-0.5, -0.5, 0.0],
                [0.5, -0.5, 0.0],
                [0.5, 0.5, 0.0],
                [-0.5, 0.5, 0.0],
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            uv: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            skin_joints: Vec::new(),
            skin_weights: Vec::new(),
            morph_targets: Vec::new(),
        }),
        ..Mesh::default()
    };
    // 2x1 texture: left red, right green.
    let texture = TextureBlob {
        width: 2,
        height: 1,
        rgb565: vec![0x00, 0xF8, 0xE0, 0x07],
    };
    let package = DevicePackage {
        target: "linux-mesh".into(),
        mode: PackageMode::Mesh,
        nodes: vec![Node {
            name: "root".into(),
            mesh: Some(0),
            ..Node::default()
        }],
        meshes: vec![mesh],
        textures: Vec::new(),
        materials: vec![Material {
            name: "skin".into(),
            texture: None,
            // White base colour: the tint is the identity, so the texture shows.
            color: Some([255, 255, 255]),
        }],
        skeletons: Vec::new(),
        animations: Vec::new(),
        sprites: Vec::new(),
        layers: Vec::new(),
        estimated_bytes: 48,
        atlas: None,
        texture: Some(texture),
    };
    let sealed = mdc_model::MdcPackage::seal(package).unwrap();

    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 16,
        height: 16,
        renderer_mesh2d: true,
        renderer_max_vertices: 4096,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    session
        .install_package(&sealed.to_bytes().unwrap())
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_preview());
    // The texture (not the blue material colour) is sampled across the quad.
    assert_eq!(device.pixel_rgb565(2, 8), Some(0xF800), "left half red");
    assert_eq!(device.pixel_rgb565(13, 8), Some(0x07E0), "right half green");
}

#[test]
fn scene_circle_and_line_edge_cases_do_not_panic() {
    use mdc_core::{ConnectionState, Session};
    use mdc_protocol::SceneNode;
    use mdc_simulator::{BoardProfile, FakeDevice};

    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 8,
        height: 8,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

    let node = |name: &str, kind: &str, rect: [u16; 4]| SceneNode {
        name: name.into(),
        kind: kind.into(),
        rect: Some(rect),
        children: Vec::new(),
        value: Some("#00FF00".into()),
    };
    // Zero-size circle and a degenerate (single-point) line must not panic and
    // must not draw outside the framebuffer.
    let scene = SceneNode {
        name: "root".into(),
        kind: "group".into(),
        rect: None,
        value: None,
        children: vec![
            node("empty", "circle", [0, 0, 0, 0]),
            node("dot", "line", [3, 3, 3, 3]),
            node("far", "line", [100, 100, 200, 200]),
        ],
    };
    session.set_scene(scene).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_scene());
    assert_eq!(
        device.pixel_rgb565(3, 3),
        Some(0x07E0),
        "single-point line drawn"
    );
    assert_eq!(
        device.pixel_rgb565(0, 0),
        Some(0),
        "empty circle draws nothing"
    );
}

#[test]
fn http_mirror_serves_a_device_frame_to_a_loopback_client() {
    use mdc_core::{ConnectionState, Session};
    use mdc_simulator::{BoardProfile, FakeDevice};
    use std::io::{Read, Write};
    use std::time::Duration;

    let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    session.start_mirror("main", "full-frame", 15).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();

    let mut http = mdc_mirror::HttpMirrorServer::bind("127.0.0.1:0").unwrap();
    for _ in 0..5 {
        device.emit_mirror_frame().unwrap();
        session.poll().unwrap();
        if let Some(frame) = session.take_mirror_frame() {
            http.publish(&mdc_mirror::ShadowFrame {
                width: frame.width,
                height: frame.height,
                generation: 1,
                frame_id: frame.frame_id,
                pixels: &frame.bytes,
            });
        }
        let _ = http.pump();
    }
    assert!(http.has_frame());

    let addr = http.local_addr().unwrap();
    let mut client = std::net::TcpStream::connect(addr).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    client
        .write_all(b"GET /frame.bmp HTTP/1.1\r\nHost: x\r\n\r\n")
        .unwrap();
    let mut response = Vec::new();
    for _ in 0..400 {
        let _ = http.pump();
        let mut buffer = [0u8; 8192];
        match client.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => response.extend_from_slice(&buffer[..n]),
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    let text = String::from_utf8_lossy(&response);
    assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
    assert!(text.contains("image/bmp"));
    assert!(response.windows(2).any(|w| w == b"BM"), "BMP body present");
}

#[test]
fn scene_group_geometry_and_named_colour_reach_the_device() {
    use mdc_core::{ConnectionState, Session};
    use mdc_protocol::SceneNode;
    use mdc_simulator::{BoardProfile, FakeDevice};

    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 16,
        height: 16,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

    let scene = SceneNode {
        name: "root".into(),
        kind: "group".into(),
        rect: Some([2, 2, 0, 0]),
        children: vec![SceneNode {
            name: "bg".into(),
            kind: "rect".into(),
            rect: Some([0, 0, 4, 4]),
            children: Vec::new(),
            value: Some("red".into()),
        }],
        value: None,
    };
    session.set_scene(scene).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_scene());
    assert_eq!(
        device.pixel_rgb565(2, 2),
        Some(0xF800),
        "group offset + named colour"
    );
    assert_eq!(device.pixel_rgb565(0, 0), Some(0));
}

#[test]
fn morph_weight_edge_cases_are_bounded() {
    use mdc_model::{Animation, AnimationTrack};
    // A weights channel whose value count is not a multiple of its key count is
    // refused (no guessing a stride).
    let ragged = Animation {
        name: "ragged".into(),
        channels: 1,
        tracks: vec![AnimationTrack {
            node: 0,
            path: "weights".into(),
            interpolation: "LINEAR".into(),
            times: vec![0.0, 1.0],
            values: vec![0.0, 1.0, 0.5],
        }],
    };
    assert!(mdc_bake::morph_weights_at(&ragged, 0, 0.5).is_none());

    // Extra weights beyond the target count are ignored, missing ones are zero.
    let base = [[0.0, 0.0, 0.0]];
    let targets = vec![vec![[1.0, 0.0, 0.0]]];
    assert_eq!(
        mdc_bake::apply_morphs(&base, &targets, &[1.0, 9.0]),
        vec![[1.0, 0.0, 0.0]]
    );
    assert_eq!(
        mdc_bake::apply_morphs(&base, &targets, &[]),
        vec![[0.0, 0.0, 0.0]]
    );
}
