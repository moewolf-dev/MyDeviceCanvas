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
                preferred_tile_size: 0,
                supports_partial_update: false,
                supports_full_framebuffer: false,
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
            boot_id: 0,
            run_id: 0,
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
        boot_id: 0,
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
        lights: Vec::new(),
        skeletons: Vec::new(),
        animations: Vec::new(),
        sprites: Vec::new(),
        layers: Vec::new(),
        estimated_bytes: pixels as u64,
        atlas: Some(atlas),
        texture: None,
        texture_blobs: Vec::new(),
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
        opacity: None,
        value2: None,
    };
    for i in 0..5 {
        node = SceneNode {
            name: format!("n{i}"),
            kind: "rect".into(),
            rect: None,
            children: vec![node],
            value: None,
            opacity: None,
            value2: None,
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
        lights: Vec::new(),
        skeletons: Vec::new(),
        animations: Vec::new(),
        sprites: Vec::new(),
        layers: Vec::new(),
        estimated_bytes: 16,
        atlas: Some(atlas),
        texture: None,
        texture_blobs: Vec::new(),
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
            opacity: None,
            value2: None,
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
            opacity: None,
            value2: None,
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
        uv2: Vec::new(),
        normals: Vec::new(),
        skin_joints: Vec::new(),
        skin_joints2: Vec::new(),
        skin_weights2: Vec::new(),
        skin_weights: Vec::new(),
        morph_targets: Vec::new(),
        colors: Vec::new(),
        tangents: Vec::new(),
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
        lights: Vec::new(),
        skeletons: Vec::new(),
        animations: Vec::new(),
        sprites: Vec::new(),
        layers: Vec::new(),
        estimated_bytes: 48,
        atlas: None,
        texture: None,
        texture_blobs: Vec::new(),
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
            uv2: Vec::new(),
            normals: Vec::new(),
            skin_joints: Vec::new(),
            skin_joints2: Vec::new(),
            skin_weights2: Vec::new(),
            skin_weights: Vec::new(),
            morph_targets: Vec::new(),
            colors: Vec::new(),
            tangents: Vec::new(),
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
            ..Default::default()
        }],
        lights: Vec::new(),
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
            alpha: Vec::new(),
        }),
        texture_blobs: Vec::new(),
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
fn packaged_masked_texture_cuts_out_on_render() {
    use mdc_model::{DevicePackage, Material, Mesh, MeshGeometry, Node, PackageMode, TextureBlob};

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
            uv2: Vec::new(),
            normals: Vec::new(),
            skin_joints: Vec::new(),
            skin_joints2: Vec::new(),
            skin_weights2: Vec::new(),
            skin_weights: Vec::new(),
            morph_targets: Vec::new(),
            colors: Vec::new(),
            tangents: Vec::new(),
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
            name: "cut".into(),
            texture: None,
            color: Some([255, 255, 255]),
            alpha_mode: Some("MASK".into()),
            alpha_cutoff: Some(0.5),
            alpha: None,
            metallic: None,
            roughness: None,
            emissive: None,
            unlit: None,
            uv_transform: None,
            emissive_strength: None,
            occlusion_texture: None,
            occlusion_strength: None,
            normal_texture: None,
            normal_scale: None,
            uv_set: None,
            emissive_texture: None,
            emissive_uv_transform: None,
            normal_uv_transform: None,
            occlusion_uv_transform: None,
            roughness_texture: None,
            roughness_uv_transform: None,
            specular_factor: None,
            specular_color: None,
            ior: None,
        }],
        lights: Vec::new(),
        skeletons: Vec::new(),
        animations: Vec::new(),
        sprites: Vec::new(),
        layers: Vec::new(),
        estimated_bytes: 48,
        atlas: None,
        texture: Some(TextureBlob {
            width: 2,
            height: 1,
            rgb565: vec![0x00, 0xF8, 0xE0, 0x07],
            alpha: vec![0b01],
        }),
        texture_blobs: Vec::new(),
    };
    let frame = mdc_bake::render_package_scene(&package, 0.0, (16, 16)).unwrap();
    let at = |x: usize, y: usize| {
        u16::from_le_bytes([
            frame.rgb565[(y * 16 + x) * 2],
            frame.rgb565[(y * 16 + x) * 2 + 1],
        ])
    };
    assert_eq!(at(2, 8), 0xF800, "opaque half drawn");
    assert_eq!(at(13, 8), 0, "transparent half clear");
}

#[test]
fn masked_material_cuts_out_transparent_texels() {
    use mdc_model::{Material, Mesh, MeshGeometry, Model, Node};

    let model = Model {
        source: "test".into(),
        nodes: vec![Node {
            name: "root".into(),
            mesh: Some(0),
            ..Node::default()
        }],
        meshes: vec![Mesh {
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
                uv2: Vec::new(),
                normals: Vec::new(),
                skin_joints: Vec::new(),
                skin_joints2: Vec::new(),
                skin_weights2: Vec::new(),
                skin_weights: Vec::new(),
                morph_targets: Vec::new(),
                colors: Vec::new(),
                tangents: Vec::new(),
            }),
            ..Mesh::default()
        }],
        materials: vec![Material {
            name: "cut".into(),
            texture: Some(0),
            color: Some([255, 255, 255]),
            alpha_mode: Some("MASK".into()),
            alpha_cutoff: Some(0.5),
            alpha: None,
            metallic: None,
            roughness: None,
            emissive: None,
            unlit: None,
            uv_transform: None,
            emissive_strength: None,
            occlusion_texture: None,
            occlusion_strength: None,
            normal_texture: None,
            normal_scale: None,
            uv_set: None,
            emissive_texture: None,
            emissive_uv_transform: None,
            normal_uv_transform: None,
            occlusion_uv_transform: None,
            roughness_texture: None,
            roughness_uv_transform: None,
            specular_factor: None,
            specular_color: None,
            ior: None,
        }],
        ..Model::default()
    };
    // 2×1 texture: left red opaque, right green transparent.
    let texture = mdc_bake::TextureMap::from_rgb565(2, 1, vec![0x00, 0xF8, 0xE0, 0x07])
        .unwrap()
        .with_alpha(vec![0b01]);
    let frame =
        mdc_bake::render_scene_at(&model, 0, 0.0, (16, 16), Some(&texture), 0x001F).unwrap();
    let at = |x: usize, y: usize| {
        u16::from_le_bytes([
            frame.rgb565[(y * 16 + x) * 2],
            frame.rgb565[(y * 16 + x) * 2 + 1],
        ])
    };
    assert_eq!(at(2, 8), 0xF800, "opaque left drawn");
    assert_eq!(at(13, 8), 0, "transparent right left clear");
}

#[test]
fn merged_multi_primitive_geometry_renders_every_triangle() {
    use mdc_model::{Material, Mesh, MeshGeometry, Model, Node};

    let model = Model {
        source: "test".into(),
        nodes: vec![Node {
            name: "root".into(),
            mesh: Some(0),
            ..Node::default()
        }],
        meshes: vec![Mesh {
            name: "body".into(),
            vertices: 6,
            indices: 6,
            primitives: 2,
            material: Some(0),
            geometry: Some(MeshGeometry {
                positions: vec![
                    [-0.9, -0.6, 0.0],
                    [-0.3, -0.6, 0.0],
                    [-0.6, 0.6, 0.0],
                    [0.3, -0.6, 0.0],
                    [0.9, -0.6, 0.0],
                    [0.6, 0.6, 0.0],
                ],
                indices: vec![0, 1, 2, 3, 4, 5],
                uv: Vec::new(),
                uv2: Vec::new(),
                normals: Vec::new(),
                skin_joints: Vec::new(),
                skin_joints2: Vec::new(),
                skin_weights2: Vec::new(),
                skin_weights: Vec::new(),
                morph_targets: Vec::new(),
                colors: Vec::new(),
                tangents: Vec::new(),
            }),
            ..Mesh::default()
        }],
        materials: vec![Material {
            name: "red".into(),
            texture: None,
            color: Some([255, 0, 0]),
            ..Default::default()
        }],
        ..Model::default()
    };
    let frame = mdc_bake::render_scene_at(&model, 0, 0.0, (32, 16), None, 0xFFFF).unwrap();
    let at = |x: usize, y: usize| {
        u16::from_le_bytes([
            frame.rgb565[(y * 32 + x) * 2],
            frame.rgb565[(y * 32 + x) * 2 + 1],
        ])
    };
    let covered: usize = (0..16)
        .flat_map(|y| (0..32).map(move |x| (x, y)))
        .filter(|(x, y)| at(*x, *y) != 0)
        .count();
    assert!(covered > 60, "two triangles cover area ({covered})");
    // Both halves have pixels (left and right triangles).
    assert!((0..16).any(|x| at(x, 8) != 0), "left triangle drawn");
    assert!((16..32).any(|x| at(x, 8) != 0), "right triangle drawn");
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
                uv2: Vec::new(),
                normals: Vec::new(),
                skin_joints: Vec::new(),
                skin_joints2: Vec::new(),
                skin_weights2: Vec::new(),
                skin_weights: Vec::new(),
                morph_targets: vec![vec![
                    [0.6, 0.0, 0.0],
                    [0.6, 0.0, 0.0],
                    [0.6, 0.0, 0.0],
                    [0.6, 0.0, 0.0],
                ]],
                colors: Vec::new(),
                tangents: Vec::new(),
            }),
            morph_weights: vec![0.0],
            ..Mesh::default()
        }],
        materials: vec![Material {
            name: "red".into(),
            texture: None,
            color: Some([255, 0, 0]),
            ..Default::default()
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
            uv2: Vec::new(),
            normals: Vec::new(),
            skin_joints: Vec::new(),
            skin_joints2: Vec::new(),
            skin_weights2: Vec::new(),
            skin_weights: Vec::new(),
            morph_targets: Vec::new(),
            colors: Vec::new(),
            tangents: Vec::new(),
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
            ..Default::default()
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
fn device_mesh_package_selects_a_named_clip() {
    use mdc_core::{ConnectionState, Session};
    use mdc_model::{Animation, AnimationTrack, Material, Mesh, MeshGeometry, Model, Node};
    use mdc_simulator::{BoardProfile, FakeDevice};

    let mesh = Mesh {
        name: "body".into(),
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
            uv2: Vec::new(),
            normals: Vec::new(),
            skin_joints: Vec::new(),
            skin_joints2: Vec::new(),
            skin_weights2: Vec::new(),
            skin_weights: Vec::new(),
            morph_targets: Vec::new(),
            colors: Vec::new(),
            tangents: Vec::new(),
        }),
        ..Mesh::default()
    };
    let clip = |name: &str, x: f32| Animation {
        name: name.into(),
        channels: 1,
        tracks: vec![AnimationTrack {
            node: 1,
            path: "translation".into(),
            interpolation: "LINEAR".into(),
            times: vec![0.0, 1.0],
            values: vec![x, 0.0, 0.0, x, 0.0, 0.0],
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
                name: "body".into(),
                mesh: Some(0),
                ..Node::default()
            },
        ],
        meshes: vec![mesh],
        materials: vec![Material {
            name: "red".into(),
            color: Some([255, 0, 0]),
            ..Default::default()
        }],
        animations: vec![clip("left", -0.6), clip("right", 0.6)],
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

    let mirror_after_fade =
        |session: &mut Session<MemoryLink>, device: &mut FakeDevice| -> Vec<u8> {
            // N330: advancing past the cross-fade window gives the pure new clip.
            device.clock.advance(400);
            device.emit_mirror_frame().unwrap();
            session.poll().unwrap();
            session.take_mirror_frame().expect("mirror frame").bytes
        };

    // N315: select the second clip by name; the device pose changes.
    assert!(session.set_animation("left").unwrap() > 0);
    device.poll().unwrap();
    session.poll().unwrap();
    let left = mirror_after_fade(&mut session, &mut device);
    assert!(session.set_animation("right").unwrap() > 0);
    device.poll().unwrap();
    session.poll().unwrap();
    let right = mirror_after_fade(&mut session, &mut device);
    assert_ne!(left, right, "selecting a different clip moved the mesh");

    // An unknown clip name is refused and keeps the previous selection.
    assert!(session.set_animation("nope").unwrap() > 0);
    device.poll().unwrap();
    let _ = session.poll();
    assert_eq!(device.mesh_clip(), mdc_bake::SceneClip::Index(1));
}

#[test]
fn multi_texture_mesh_package_renders_on_the_device() {
    use mdc_core::{ConnectionState, Session};
    use mdc_model::{Material, Mesh, MeshGeometry, Model, Node, Texture};
    use mdc_simulator::{BoardProfile, FakeDevice};

    let png = |color: [u8; 3]| {
        let image = image::RgbImage::from_pixel(1, 1, image::Rgb(color));
        let mut out = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    };
    let quad = || MeshGeometry {
        positions: vec![
            [-0.5, -0.5, 0.0],
            [0.5, -0.5, 0.0],
            [0.5, 0.5, 0.0],
            [-0.5, 0.5, 0.0],
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
        uv: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        uv2: Vec::new(),
        normals: Vec::new(),
        skin_joints: Vec::new(),
        skin_joints2: Vec::new(),
        skin_weights2: Vec::new(),
        skin_weights: Vec::new(),
        morph_targets: Vec::new(),
        colors: Vec::new(),
        tangents: Vec::new(),
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
        meshes: vec![
            Mesh {
                name: "left".into(),
                vertices: 4,
                indices: 6,
                material: Some(0),
                geometry: Some(quad()),
                ..Mesh::default()
            },
            Mesh {
                name: "right".into(),
                vertices: 4,
                indices: 6,
                material: Some(1),
                geometry: Some(quad()),
                ..Mesh::default()
            },
        ],
        materials: vec![
            Material {
                name: "left".into(),
                texture: Some(0),
                color: Some([255, 255, 255]),
                ..Material::default()
            },
            Material {
                name: "right".into(),
                texture: Some(1),
                color: Some([255, 255, 255]),
                ..Material::default()
            },
        ],
        textures: vec![
            Texture {
                name: "red".into(),
                width: 1,
                height: 1,
                format: "image/png".into(),
                bytes: 0,
                data: Some(png([255, 0, 0])),
                wrap: None,
            },
            Texture {
                name: "blue".into(),
                width: 1,
                height: 1,
                format: "image/png".into(),
                bytes: 0,
                data: Some(png([0, 0, 255])),
                wrap: None,
            },
        ],
        ..Model::default()
    };
    let blobs = mdc_bake::attach_model_textures(&model, &mdc_bake::BakeLimits::default());
    let package = mdc_bake::mesh_package_with_textures(
        &model,
        &mdc_model::RendererCapability::full_3d(),
        blobs,
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
    let frame = session.take_mirror_frame().expect("mirror frame");
    assert_eq!((frame.width, frame.height), (800, 480));
    let at = |x: usize, y: usize| {
        u16::from_le_bytes([
            frame.bytes[(y * 800 + x) * 2],
            frame.bytes[(y * 800 + x) * 2 + 1],
        ])
    };
    let left = at(200, 240);
    let right = at(600, 240);
    assert!(
        left >> 11 > 20 && (left & 0x1f) < 8,
        "left red: {left:#06x}"
    );
    assert!(
        (right >> 11) < 8 && (right & 0x1f) > 20,
        "right blue: {right:#06x}"
    );
}

#[test]
fn unlit_material_and_vertex_colours_render_on_the_device() {
    use mdc_core::{ConnectionState, Session};
    use mdc_model::{Material, Mesh, MeshGeometry, Model, Node};
    use mdc_simulator::{BoardProfile, FakeDevice};

    /// Render a single white quad (side-facing normals, per-vertex red→blue
    /// colour) and return the top-left pixel, which is dominated by the
    /// vertex-coloured red edge.
    fn render_quad(unlit: bool) -> u16 {
        let geometry = MeshGeometry {
            positions: vec![
                [-0.5, -0.5, 0.0],
                [0.5, -0.5, 0.0],
                [0.5, 0.5, 0.0],
                [-0.5, 0.5, 0.0],
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            uv: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            uv2: Vec::new(),
            normals: vec![[1.0, 0.0, 0.0]; 4],
            colors: vec![
                [1.0, 0.0, 0.0, 1.0],
                [0.0, 0.0, 1.0, 1.0],
                [0.0, 0.0, 1.0, 1.0],
                [1.0, 0.0, 0.0, 1.0],
            ],
            tangents: Vec::new(),
            skin_joints: Vec::new(),
            skin_joints2: Vec::new(),
            skin_weights2: Vec::new(),
            skin_weights: Vec::new(),
            morph_targets: Vec::new(),
        };
        let model = Model {
            source: "test".into(),
            nodes: vec![Node {
                name: "root".into(),
                mesh: Some(0),
                ..Node::default()
            }],
            meshes: vec![Mesh {
                name: "quad".into(),
                vertices: 4,
                indices: 6,
                material: Some(0),
                geometry: Some(geometry),
                ..Mesh::default()
            }],
            materials: vec![Material {
                name: "flat".into(),
                color: Some([255, 255, 255]),
                unlit: Some(unlit),
                uv_transform: None,
                emissive_strength: None,
                occlusion_texture: None,
                occlusion_strength: None,
                normal_texture: None,
                normal_scale: None,
                uv_set: None,
                emissive_texture: None,
                emissive_uv_transform: None,
                normal_uv_transform: None,
                occlusion_uv_transform: None,
                roughness_texture: None,
                roughness_uv_transform: None,
                specular_factor: None,
                specular_color: None,
                ior: None,
                ..Material::default()
            }],
            ..Model::default()
        };
        let package = mdc_bake::mesh_package_with_textures(
            &model,
            &mdc_model::RendererCapability::full_3d(),
            Vec::new(),
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
        let frame = session.take_mirror_frame().expect("mirror frame");
        let index = (240 * 800 + 240) * 2;
        u16::from_le_bytes([frame.bytes[index], frame.bytes[index + 1]])
    }

    let lit = render_quad(false);
    let unlit = render_quad(true);
    // N363: the unlit surface keeps its full brightness while the lit one darkens.
    assert!(
        unlit >> 11 > lit >> 11,
        "unlit brighter: lit={lit:#06x} unlit={unlit:#06x}"
    );
    // N364: the left edge is tinted red by the vertex colour.
    assert!(unlit >> 11 > (unlit & 0x1f), "left is red: {unlit:#06x}");
}

#[test]
fn occlusion_texture_darkens_the_shadowed_triangles() {
    use mdc_core::{ConnectionState, Session};
    use mdc_model::{Material, Mesh, MeshGeometry, Model, Node, Texture};
    use mdc_simulator::{BoardProfile, FakeDevice};

    let png = |w: u32, h: u32, pixels: Vec<u8>| {
        let image = image::RgbImage::from_raw(w, h, pixels).unwrap();
        let mut out = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    };
    let geometry = || MeshGeometry {
        positions: vec![
            [-0.5, -0.5, 0.0],
            [0.5, -0.5, 0.0],
            [0.5, 0.5, 0.0],
            [-0.5, 0.5, 0.0],
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
        uv: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        uv2: Vec::new(),
        normals: Vec::new(),
        skin_joints: Vec::new(),
        skin_joints2: Vec::new(),
        skin_weights2: Vec::new(),
        skin_weights: Vec::new(),
        morph_targets: Vec::new(),
        colors: Vec::new(),
        tangents: Vec::new(),
    };
    let build = |occlusion: Option<usize>| Model {
        source: "test".into(),
        nodes: vec![Node {
            name: "root".into(),
            mesh: Some(0),
            ..Node::default()
        }],
        meshes: vec![Mesh {
            name: "quad".into(),
            vertices: 4,
            indices: 6,
            material: Some(0),
            geometry: Some(geometry()),
            ..Mesh::default()
        }],
        materials: vec![Material {
            name: "lit".into(),
            texture: Some(0),
            color: Some([255, 255, 255]),
            // N378: occlusion map is texture 1 (left black, right white).
            occlusion_texture: occlusion,
            occlusion_strength: Some(1.0),
            normal_texture: None,
            normal_scale: None,
            uv_set: None,
            emissive_texture: None,
            emissive_uv_transform: None,
            normal_uv_transform: None,
            occlusion_uv_transform: None,
            roughness_texture: None,
            roughness_uv_transform: None,
            specular_factor: None,
            specular_color: None,
            ior: None,
            ..Material::default()
        }],
        textures: vec![
            Texture {
                name: "white".into(),
                width: 1,
                height: 1,
                format: "image/png".into(),
                bytes: 0,
                data: Some(png(1, 1, vec![255, 255, 255])),
                wrap: None,
            },
            Texture {
                name: "ao".into(),
                width: 2,
                height: 1,
                format: "image/png".into(),
                bytes: 0,
                data: Some(png(2, 1, vec![0, 0, 0, 255, 255, 255])),
                wrap: None,
            },
        ],
        ..Model::default()
    };

    // Render the model on the device and return the mirrored frame.
    let render = |occlusion: Option<usize>| -> Vec<u8> {
        let model = build(occlusion);
        let blobs = mdc_bake::attach_model_textures(&model, &mdc_bake::BakeLimits::default());
        let package = mdc_bake::mesh_package_with_textures(
            &model,
            &mdc_model::RendererCapability::full_3d(),
            blobs,
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
        session.take_mirror_frame().expect("mirror frame").bytes
    };
    // Mean brightness of the drawn (non-background) pixels.
    let mean_drawn = |bytes: &[u8]| -> f64 {
        let background = u16::from_le_bytes([bytes[0], bytes[1]]);
        let mut sum = 0.0f64;
        let mut count = 0u32;
        for i in (0..bytes.len()).step_by(2) {
            let value = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
            if value != background {
                sum += f64::from(value >> 5);
                count += 1;
            }
        }
        sum / f64::from(count.max(1))
    };
    let without = mean_drawn(&render(None));
    let with_ao = mean_drawn(&render(Some(1)));
    assert!(
        with_ao < without,
        "occlusion darkens: with={with_ao:.2} without={without:.2}"
    );
}

#[test]
fn normal_map_perturbs_per_pixel_shading() {
    use mdc_core::{ConnectionState, Session};
    use mdc_model::{Material, Mesh, MeshGeometry, Model, Node, Texture};
    use mdc_simulator::{BoardProfile, FakeDevice};

    let png = |w: u32, h: u32, pixels: Vec<u8>| {
        let image = image::RgbImage::from_raw(w, h, pixels).unwrap();
        let mut out = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    };
    let geometry = || MeshGeometry {
        positions: vec![
            [-0.5, -0.5, 0.0],
            [0.5, -0.5, 0.0],
            [0.5, 0.5, 0.0],
            [-0.5, 0.5, 0.0],
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
        uv: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        uv2: Vec::new(),
        normals: vec![[0.0, 0.0, 1.0]; 4],
        // N379: tangent along +x, right-handed.
        tangents: vec![[1.0, 0.0, 0.0, 1.0]; 4],
        colors: Vec::new(),
        skin_joints: Vec::new(),
        skin_joints2: Vec::new(),
        skin_weights2: Vec::new(),
        skin_weights: Vec::new(),
        morph_targets: Vec::new(),
    };
    let build = |normal: Option<usize>| Model {
        source: "test".into(),
        nodes: vec![Node {
            name: "root".into(),
            mesh: Some(0),
            ..Node::default()
        }],
        meshes: vec![Mesh {
            name: "quad".into(),
            vertices: 4,
            indices: 6,
            material: Some(0),
            geometry: Some(geometry()),
            ..Mesh::default()
        }],
        materials: vec![Material {
            name: "lit".into(),
            texture: Some(0),
            color: Some([255, 255, 255]),
            // N379: normal map is texture 1 (left tilts away, right faces the light).
            normal_texture: normal,
            normal_scale: Some(1.0),
            uv_set: None,
            emissive_texture: None,
            emissive_uv_transform: None,
            normal_uv_transform: None,
            occlusion_uv_transform: None,
            roughness_texture: None,
            roughness_uv_transform: None,
            specular_factor: None,
            specular_color: None,
            ior: None,
            ..Material::default()
        }],
        textures: vec![
            Texture {
                name: "white".into(),
                width: 1,
                height: 1,
                format: "image/png".into(),
                bytes: 0,
                data: Some(png(1, 1, vec![255, 255, 255])),
                wrap: None,
            },
            Texture {
                name: "normal".into(),
                width: 2,
                height: 1,
                format: "image/png".into(),
                bytes: 0,
                data: Some(png(2, 1, vec![255, 0, 0, 128, 128, 255])),
                wrap: None,
            },
        ],
        ..Model::default()
    };
    let render = |normal: Option<usize>| -> Vec<u8> {
        let model = build(normal);
        let blobs = mdc_bake::attach_model_textures(&model, &mdc_bake::BakeLimits::default());
        let package = mdc_bake::mesh_package_with_textures(
            &model,
            &mdc_model::RendererCapability::full_3d(),
            blobs,
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
        session.take_mirror_frame().expect("mirror frame").bytes
    };
    let mean_drawn = |bytes: &[u8]| -> f64 {
        let background = u16::from_le_bytes([bytes[0], bytes[1]]);
        let mut sum = 0.0f64;
        let mut count = 0u32;
        for i in (0..bytes.len()).step_by(2) {
            let value = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
            if value != background {
                sum += f64::from(value >> 5);
                count += 1;
            }
        }
        sum / f64::from(count.max(1))
    };
    let flat = mean_drawn(&render(None));
    let mapped = mean_drawn(&render(Some(1)));
    assert!(
        mapped < flat,
        "normal map darkens the tilted triangle: mapped={mapped:.2} flat={flat:.2}"
    );
}

#[test]
fn packed_material_atlas_renders_on_the_device() {
    use mdc_core::{ConnectionState, Session};
    use mdc_model::{Material, Mesh, MeshGeometry, Model, Node, Texture};
    use mdc_simulator::{BoardProfile, FakeDevice};

    let png = |w: u32, h: u32, pixels: Vec<u8>| {
        let image = image::RgbImage::from_raw(w, h, pixels).unwrap();
        let mut out = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    };
    let model = Model {
        source: "test".into(),
        nodes: vec![Node {
            name: "root".into(),
            mesh: Some(0),
            ..Node::default()
        }],
        meshes: vec![Mesh {
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
                uv2: Vec::new(),
                normals: Vec::new(),
                colors: Vec::new(),
                tangents: Vec::new(),
                skin_joints: Vec::new(),
                skin_joints2: Vec::new(),
                skin_weights2: Vec::new(),
                skin_weights: Vec::new(),
                morph_targets: Vec::new(),
            }),
            ..Mesh::default()
        }],
        materials: vec![Material {
            name: "m".into(),
            texture: Some(0),
            color: Some([255, 255, 255]),
            ..Material::default()
        }],
        textures: vec![
            Texture {
                name: "red-green".into(),
                width: 2,
                height: 1,
                format: "image/png".into(),
                bytes: 0,
                data: Some(png(2, 1, vec![255, 0, 0, 0, 255, 0])),
                wrap: None,
            },
            Texture {
                name: "extra".into(),
                width: 2,
                height: 1,
                format: "image/png".into(),
                bytes: 0,
                data: Some(png(2, 1, vec![0, 0, 255, 255, 255, 255])),
                wrap: None,
            },
        ],
        ..Model::default()
    };
    let blobs = mdc_bake::attach_model_textures(&model, &mdc_bake::BakeLimits::default());
    let mut package = mdc_bake::mesh_package_with_textures(
        &model,
        &mdc_model::RendererCapability::full_3d(),
        blobs,
    )
    .unwrap();
    // N380: collapse the two textures into one page.
    let mut inner = package.package.clone();
    assert!(mdc_bake::pack_material_atlas(&mut inner, 64), "packed");
    package = mdc_model::MdcPackage::seal(inner).unwrap();

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
    let frame = session.take_mirror_frame().expect("mirror frame");
    let at = |x: usize, y: usize| {
        u16::from_le_bytes([
            frame.bytes[(y * 800 + x) * 2],
            frame.bytes[(y * 800 + x) * 2 + 1],
        ])
    };
    let left = at(200, 240);
    let right = at(600, 240);
    assert!(
        left >> 11 > ((left >> 5) & 0x3f),
        "left is red: {left:#06x}"
    );
    assert!(
        ((right >> 5) & 0x3f) > right >> 11,
        "right is green: {right:#06x}"
    );
}

#[test]
fn smooth_shading_gradients_across_a_triangle() {
    use mdc_model::{Material, Mesh, MeshGeometry, Model, Node};

    // Normals vary across the quad, so per-pixel (smooth) shading differs from
    // the per-triangle (flat) shade.
    let model = Model {
        source: "test".into(),
        nodes: vec![Node {
            name: "root".into(),
            mesh: Some(0),
            ..Node::default()
        }],
        meshes: vec![Mesh {
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
                uv: Vec::new(),
                uv2: Vec::new(),
                normals: vec![
                    [0.0, 0.0, 1.0],
                    [1.0, 0.0, 0.0],
                    [1.0, 0.0, 0.0],
                    [0.0, 0.0, 1.0],
                ],
                colors: Vec::new(),
                tangents: Vec::new(),
                skin_joints: Vec::new(),
                skin_weights: Vec::new(),
                skin_joints2: Vec::new(),
                skin_weights2: Vec::new(),
                morph_targets: Vec::new(),
            }),
            ..Mesh::default()
        }],
        materials: vec![Material {
            name: "m".into(),
            color: Some([255, 255, 255]),
            ..Material::default()
        }],
        ..Model::default()
    };
    let package =
        mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::full_3d()).unwrap();
    let flat = mdc_bake::render_package_scene(&package.package, 0.0, (32, 32)).unwrap();
    let smooth = mdc_bake::render_package_scene_clip_mode(
        &package.package,
        mdc_bake::SceneClip::First,
        0.0,
        (32, 32),
        mdc_bake::Filter::Nearest,
        mdc_bake::ShadeMode::Smooth,
    )
    .unwrap();
    assert_ne!(
        flat.rgb565, smooth.rgb565,
        "smooth shading differs from flat"
    );
}

#[test]
fn smooth_specular_adds_a_highlight() {
    use mdc_model::{Material, Mesh, MeshGeometry, Model, Node};

    let model = Model {
        source: "test".into(),
        nodes: vec![Node {
            name: "root".into(),
            mesh: Some(0),
            ..Node::default()
        }],
        meshes: vec![Mesh {
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
                uv: Vec::new(),
                uv2: Vec::new(),
                normals: vec![[0.0, 0.0, 1.0]; 4],
                colors: Vec::new(),
                tangents: Vec::new(),
                skin_joints: Vec::new(),
                skin_weights: Vec::new(),
                skin_joints2: Vec::new(),
                skin_weights2: Vec::new(),
                morph_targets: Vec::new(),
            }),
            ..Mesh::default()
        }],
        materials: vec![Material {
            name: "shiny".into(),
            color: Some([128, 128, 128]),
            metallic: Some(1.0),
            roughness: Some(0.1),
            ..Material::default()
        }],
        ..Model::default()
    };
    let package =
        mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::full_3d()).unwrap();
    let render = |mode| {
        mdc_bake::render_package_scene_clip_mode(
            &package.package,
            mdc_bake::SceneClip::First,
            0.0,
            (32, 32),
            mdc_bake::Filter::Nearest,
            mode,
        )
        .unwrap()
    };
    let smooth = render(mdc_bake::ShadeMode::Smooth);
    let spec = render(mdc_bake::ShadeMode::SmoothSpecular);
    assert_ne!(smooth.rgb565, spec.rgb565, "specular changes the frame");
    let sum = |f: &mdc_bake::BakedTexture| -> u64 {
        f.rgb565
            .chunks_exact(2)
            .map(|b| u64::from(u16::from_le_bytes([b[0], b[1]])) >> 5)
            .sum()
    };
    assert!(sum(&spec) > sum(&smooth), "highlight brightens the surface");
}

#[test]
fn imported_directional_light_reaches_the_device_render() {
    use mdc_core::{ConnectionState, Session};
    use mdc_model::{Light, Material, Mesh, MeshGeometry, Model, Node};
    use mdc_simulator::{BoardProfile, FakeDevice};

    let build = |lights: Vec<Light>| Model {
        source: "test".into(),
        nodes: vec![Node {
            name: "root".into(),
            mesh: Some(0),
            ..Node::default()
        }],
        meshes: vec![Mesh {
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
                uv: Vec::new(),
                uv2: Vec::new(),
                normals: vec![[0.0, 0.0, 1.0]; 4],
                colors: Vec::new(),
                tangents: Vec::new(),
                skin_joints: Vec::new(),
                skin_weights: Vec::new(),
                skin_joints2: Vec::new(),
                skin_weights2: Vec::new(),
                morph_targets: Vec::new(),
            }),
            ..Mesh::default()
        }],
        materials: vec![Material {
            name: "m".into(),
            color: Some([255, 255, 255]),
            ..Material::default()
        }],
        lights,
        ..Model::default()
    };
    let render = |lights: Vec<Light>| -> Vec<u8> {
        let package = mdc_bake::mesh_package_with_textures(
            &build(lights),
            &mdc_model::RendererCapability::full_3d(),
            Vec::new(),
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
        session.take_mirror_frame().expect("mirror frame").bytes
    };
    let mean_drawn = |bytes: &[u8]| -> f64 {
        let background = u16::from_le_bytes([bytes[0], bytes[1]]);
        let mut sum = 0.0f64;
        let mut count = 0u32;
        for i in (0..bytes.len()).step_by(2) {
            let value = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
            if value != background {
                sum += f64::from(value >> 5);
                count += 1;
            }
        }
        sum / f64::from(count.max(1))
    };
    let lit = mean_drawn(&render(Vec::new()));
    let backlit = mean_drawn(&render(vec![Light {
        name: "back".into(),
        kind: "directional".into(),
        color: [255, 255, 255],
        intensity: 1.0,
        direction: [0.0, 0.0, 1.0],
    }]));
    assert!(
        backlit < lit,
        "the imported light darkens the device frame: lit={lit:.2} backlit={backlit:.2}"
    );
}

#[test]
fn emissive_texture_adds_light_on_the_device() {
    use mdc_core::{ConnectionState, Session};
    use mdc_model::{Material, Mesh, MeshGeometry, Model, Node, Texture, UvTransform};
    use mdc_simulator::{BoardProfile, FakeDevice};

    let png = |w: u32, h: u32, pixels: Vec<u8>| {
        let image = image::RgbImage::from_raw(w, h, pixels).unwrap();
        let mut out = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    };
    let build = |emissive: Option<usize>, transform: Option<UvTransform>| Model {
        source: "test".into(),
        nodes: vec![Node {
            name: "root".into(),
            mesh: Some(0),
            ..Node::default()
        }],
        meshes: vec![Mesh {
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
                uv2: Vec::new(),
                normals: vec![[0.0, 0.0, 1.0]; 4],
                colors: Vec::new(),
                tangents: Vec::new(),
                skin_joints: Vec::new(),
                skin_weights: Vec::new(),
                skin_joints2: Vec::new(),
                skin_weights2: Vec::new(),
                morph_targets: Vec::new(),
            }),
            ..Mesh::default()
        }],
        materials: vec![Material {
            name: "glow".into(),
            // Black base colour so any colour must come from the emissive map.
            texture: Some(0),
            color: Some([0, 0, 0]),
            emissive_texture: emissive,
            emissive_strength: Some(1.0),
            emissive_uv_transform: transform,
            ..Material::default()
        }],
        textures: vec![
            Texture {
                name: "black".into(),
                width: 1,
                height: 1,
                format: "image/png".into(),
                bytes: 0,
                data: Some(png(1, 1, vec![0, 0, 0])),
                wrap: None,
            },
            Texture {
                name: "glow".into(),
                width: 2,
                height: 1,
                format: "image/png".into(),
                bytes: 0,
                // Left red, right green.
                data: Some(png(2, 1, vec![255, 0, 0, 0, 255, 0])),
                wrap: None,
            },
        ],
        ..Model::default()
    };
    let render = |emissive: Option<usize>, transform: Option<UvTransform>| -> Vec<u8> {
        let model = build(emissive, transform);
        let blobs = mdc_bake::attach_model_textures(&model, &mdc_bake::BakeLimits::default());
        let package = mdc_bake::mesh_package_with_textures(
            &model,
            &mdc_model::RendererCapability::full_3d(),
            blobs,
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
        session.take_mirror_frame().expect("mirror frame").bytes
    };
    let at = |bytes: &[u8], x: usize, y: usize| {
        u16::from_le_bytes([bytes[(y * 800 + x) * 2], bytes[(y * 800 + x) * 2 + 1]])
    };

    // Emissive off: the black base colour keeps the quad dark.
    let off = render(None, None);
    // Emissive on with a half-UV offset: the left half samples the red texel.
    let on = render(
        Some(1),
        Some(UvTransform {
            offset: [0.25, 0.0],
            rotation: 0.0,
            scale: [0.5, 1.0],
        }),
    );
    let dark = at(&off, 300, 240);
    let glow = at(&on, 300, 240);
    assert!(glow >> 11 > 20, "emissive adds red: {glow:#06x}");
    assert!(
        dark >> 11 < 8,
        "base stays dark without emissive: {dark:#06x}"
    );
}

#[test]
fn roughness_texture_modulates_shading_per_texel() {
    use mdc_model::{Material, Mesh, MeshGeometry, Model, Node, Texture};

    let png = |w: u32, h: u32, pixels: Vec<u8>| {
        let image = image::RgbImage::from_raw(w, h, pixels).unwrap();
        let mut out = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    };
    let model = Model {
        source: "test".into(),
        nodes: vec![Node {
            name: "root".into(),
            mesh: Some(0),
            ..Node::default()
        }],
        meshes: vec![Mesh {
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
                uv2: Vec::new(),
                // Grazing (back-facing) normals so the ambient floor dominates.
                normals: vec![[0.0, 0.0, -1.0]; 4],
                colors: Vec::new(),
                tangents: Vec::new(),
                skin_joints: Vec::new(),
                skin_weights: Vec::new(),
                skin_joints2: Vec::new(),
                skin_weights2: Vec::new(),
                morph_targets: Vec::new(),
            }),
            ..Mesh::default()
        }],
        materials: vec![Material {
            name: "m".into(),
            color: Some([255, 255, 255]),
            // N394: left teixel rough (G=255), right smooth (G=0).
            roughness_texture: Some(0),
            ..Material::default()
        }],
        textures: vec![Texture {
            name: "rm".into(),
            width: 2,
            height: 1,
            format: "image/png".into(),
            bytes: 0,
            data: Some(png(2, 1, vec![0, 255, 0, 0, 0, 0])),
            wrap: None,
        }],
        ..Model::default()
    };
    let blobs = mdc_bake::attach_model_textures(&model, &mdc_bake::BakeLimits::default());
    let package = mdc_bake::mesh_package_with_textures(
        &model,
        &mdc_model::RendererCapability::full_3d(),
        blobs,
    )
    .unwrap();
    let frame = mdc_bake::render_package_scene(&package.package, 0.0, (32, 32)).unwrap();
    let at = |x: usize| {
        u16::from_le_bytes([
            frame.rgb565[(16 * 32 + x) * 2],
            frame.rgb565[(16 * 32 + x) * 2 + 1],
        ])
    };
    assert!(
        at(6) > at(25),
        "the rough teixel is brighter: left={:#06x} right={:#06x}",
        at(6),
        at(25)
    );
}

#[test]
fn specular_factor_scales_the_highlight() {
    use mdc_model::{Material, Mesh, MeshGeometry, Model, Node};

    let build = |specular_factor: f32| Model {
        source: "test".into(),
        nodes: vec![Node {
            name: "root".into(),
            mesh: Some(0),
            ..Node::default()
        }],
        meshes: vec![Mesh {
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
                uv: Vec::new(),
                uv2: Vec::new(),
                normals: vec![[0.0, 0.0, 1.0]; 4],
                colors: Vec::new(),
                tangents: Vec::new(),
                skin_joints: Vec::new(),
                skin_weights: Vec::new(),
                skin_joints2: Vec::new(),
                skin_weights2: Vec::new(),
                morph_targets: Vec::new(),
            }),
            ..Mesh::default()
        }],
        materials: vec![Material {
            name: "shiny".into(),
            color: Some([128, 128, 128]),
            metallic: Some(1.0),
            roughness: Some(0.1),
            // N397: KHR_materials_specular factor.
            specular_factor: Some(specular_factor),
            ..Material::default()
        }],
        ..Model::default()
    };
    let render = |specular_factor: f32| {
        let package = mdc_model::compile_to_package(
            &build(specular_factor),
            &mdc_model::RendererCapability::full_3d(),
        )
        .unwrap();
        mdc_bake::render_package_scene_clip_mode(
            &package.package,
            mdc_bake::SceneClip::First,
            0.0,
            (32, 32),
            mdc_bake::Filter::Nearest,
            mdc_bake::ShadeMode::SmoothSpecular,
        )
        .unwrap()
    };
    let sum = |f: &mdc_bake::BakedTexture| -> u64 {
        f.rgb565
            .chunks_exact(2)
            .map(|b| u64::from(u16::from_le_bytes([b[0], b[1]])) >> 5)
            .sum()
    };
    let full = render(1.0);
    let dim = render(0.1);
    assert!(
        sum(&full) > sum(&dim),
        "a smaller factor dims the highlight"
    );
}

#[test]
fn device_diagnostics_reach_the_host_channel() {
    use mdc_core::{ConnectionState, DiagnosticRecord, Session};
    use mdc_simulator::{BoardProfile, FakeDevice};

    let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

    // N316: metrics and boot info travel on their own types, not the frame path.
    device.emit_boot_info().unwrap();
    device.emit_metric("fps", 29.5, Some("fps")).unwrap();
    let mut fields = std::collections::BTreeMap::new();
    fields.insert("heap".to_string(), "1234".to_string());
    device.emit_telemetry("soak", Some("info"), fields).unwrap();
    device
        .emit_crash("load-access", Some("mdc_app"), Some(0x4000_0000))
        .unwrap();
    session.poll().unwrap();
    let records = session.take_diagnostics();
    assert_eq!(records.len(), 4);
    assert!(matches!(records[0], DiagnosticRecord::BootInfo(_)));
    assert!(matches!(records[2], DiagnosticRecord::Telemetry(_)));
    assert!(matches!(records[3], DiagnosticRecord::Crash(_)));
    match &records[1] {
        DiagnosticRecord::Metric(metric) => {
            assert_eq!(metric.name, "fps");
            assert_eq!(metric.unit.as_deref(), Some("fps"));
            assert!((metric.value - 29.5).abs() < 1e-9);
            assert_eq!(metric.boot_id, device.boot_id());
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn mirror_http_stream_pushes_device_frames() {
    use mdc_core::{ConnectionState, Session};
    use mdc_simulator::{BoardProfile, FakeDevice};
    use std::io::{Read, Write};
    use std::time::Duration;

    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 8,
        height: 4,
        mirror: true,
        mirror_fps: 15,
        device_id: "sim-http".into(),
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    session.start_mirror("main", "full-frame", 15).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();

    let mut http = mdc_mirror::HttpMirrorServer::bind("127.0.0.1:0").unwrap();
    let addr = http.local_addr().unwrap();
    let mut client = std::net::TcpStream::connect(addr).unwrap();
    client
        .set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    client
        .write_all(b"GET /stream HTTP/1.1\r\nHost: x\r\n\r\n")
        .unwrap();

    let mut response = Vec::new();
    for _ in 0..3 {
        for _ in 0..10 {
            device.emit_mirror_frame().unwrap();
            session.poll().unwrap();
            if let Some(frame) = session.take_mirror_frame() {
                http.publish(&mdc_mirror::ShadowFrame {
                    width: frame.width,
                    height: frame.height,
                    generation: frame.frame_id,
                    frame_id: frame.frame_id,
                    pixels: &frame.bytes,
                });
                break;
            }
        }
        for _ in 0..10 {
            let _ = http.pump();
            let mut buffer = [0u8; 8192];
            match client.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => response.extend_from_slice(&buffer[..n]),
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    }
    let head = String::from_utf8_lossy(&response);
    assert!(
        head.contains("multipart/x-mixed-replace; boundary=mdcframe"),
        "{head:.80}"
    );
    let parts = response.windows(10).filter(|w| *w == b"--mdcframe").count();
    assert!(parts >= 2, "expected a live stream of parts, got {parts}");
    assert_eq!(http.streaming_clients(), 1);
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
            uv2: Vec::new(),
            normals: Vec::new(),
            skin_joints: Vec::new(),
            skin_joints2: Vec::new(),
            skin_weights2: Vec::new(),
            skin_weights: Vec::new(),
            morph_targets: Vec::new(),
            colors: Vec::new(),
            tangents: Vec::new(),
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
                ..Default::default()
            },
            Material {
                name: "green".into(),
                texture: None,
                color: Some([0, 255, 0]),
                ..Default::default()
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
            uv2: Vec::new(),
            normals: Vec::new(),
            skin_joints: Vec::new(),
            skin_joints2: Vec::new(),
            skin_weights2: Vec::new(),
            skin_weights: Vec::new(),
            morph_targets: Vec::new(),
            colors: Vec::new(),
            tangents: Vec::new(),
        }),
        ..Mesh::default()
    };
    // 2x1 texture: left red, right green.
    let texture = TextureBlob {
        width: 2,
        height: 1,
        rgb565: vec![0x00, 0xF8, 0xE0, 0x07],
        alpha: Vec::new(),
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
            ..Default::default()
        }],
        lights: Vec::new(),
        skeletons: Vec::new(),
        animations: Vec::new(),
        sprites: Vec::new(),
        layers: Vec::new(),
        estimated_bytes: 48,
        atlas: None,
        texture: Some(texture),
        texture_blobs: Vec::new(),
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
        opacity: None,
        value2: None,
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
        opacity: None,
        value2: None,
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
            opacity: None,
            value2: None,
        }],
        value: None,
        opacity: None,
        value2: None,
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

// ---- N304–N308: reboot, pacing and display-profile fixtures -----------------

#[test]
fn device_reboot_drops_the_previous_run_over_the_simulator() {
    let profile = BoardProfile {
        width: 4,
        height: 2,
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    let first_boot = device.boot_id();
    assert_eq!(session.device_boot_id(), Some(first_boot));

    // An in-flight frame for this run leaves one pending ACK.
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 4,
            height: 2,
            bytes: vec![1; 16],
        })
        .unwrap();
    assert_eq!(session.pending_request_count(), 1);

    // Device reboots; the host refreshes capabilities and re-handshakes.
    device.restart();
    assert_eq!(device.boot_id(), first_boot.wrapping_add(1));
    session.refresh_capabilities().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();

    assert_eq!(session.device_boot_id(), Some(device.boot_id()));
    assert_ne!(session.device_boot_id(), Some(first_boot));
    assert_eq!(session.pending_request_count(), 0);
    assert!(session.needs_full_frame);
}

#[test]
fn scheduler_bounds_a_simulated_mirror_bandwidth() {
    let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    let device = session.device.as_ref().unwrap();
    let mirror_fps = device.capabilities.mirror_fps;
    let bandwidth = device.capabilities.mirror_bandwidth;
    assert!(mirror_fps > 0 && bandwidth > 0);

    // N305/N306: the advertised mirror limits drive one pacing budget shared by
    // every consumer. The simulator advertises one frame per mirror interval, so
    // the budget never exceeds one full 800x480 frame.
    let scheduler = mdc_schedule::Scheduler::for_mirror(mirror_fps, bandwidth);
    let full_frame = 800u64 * 480 * 2;
    assert!(scheduler.budget_bytes() > 0 && scheduler.budget_bytes() <= full_frame);
    let tile = mdc_schedule::TileCost::new(0, 32 * 32 * 2, false);
    assert!(!scheduler.plan(0, &[tile]).is_defer());
}

#[test]
fn surface_pixel_format_and_display_profile_are_exposed() {
    let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    let device = session.device.as_ref().unwrap();
    let surface = device.surface("main").expect("main surface");
    // N307: the enum is threaded through the public API; RGB565 is implemented.
    assert!(surface.pixel_format.is_implemented());
    assert_eq!(surface.pixel_format.to_string(), "RGB565");
    // N308: the host adapts to the device-advertised tile size.
    assert_eq!(surface.preferred_tile_size, 64);
    assert!(surface.supports_partial_update && surface.supports_full_framebuffer);
    let tiles = device.plan_surface_tiles("main");
    assert!(!tiles.is_empty());
    let (_, _, w, h) = tiles[0];
    assert_eq!((w, h), (64, 64));
    assert_eq!(tiles.len(), 800usize.div_ceil(64) * 480usize.div_ceil(64));
}
