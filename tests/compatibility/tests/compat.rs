//! Compatibility gates: major mismatch and low negotiated limits.
use mdc_core::{ConnectionState, CoreError, Frame, Session, MAX_FRAME_BYTES};
use mdc_protocol::{encode_control, Capabilities, MessageType, Packet, Surface, VERSION};
use mdc_simulator::{BoardProfile, FakeDevice};
use mdc_transport::MemoryLink;
use std::io::{Read, Write};

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
        mips: Vec::new(),
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
        visible: None,
        anim: None,
        rotate: None,
        blend: None,
        align: None,
        z: None,
        wrap: None,
        mode: None,
        focus: None,
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
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
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
            mode: None,
            speed_permille: None,
        }],

        mips: Vec::new(),
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
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
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
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
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

    let (link, mut device) = FakeDevice::pair(BoardProfile {
        width: 8,
        height: 8,
        renderer_mesh2d: true,
        ..BoardProfile::default()
    });
    let mut session = Session::new(link);
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
        variant_materials: Vec::new(),
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
            palette: Vec::new(),
            indices: Vec::new(),
            mips: Vec::new(),
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
            uv_set2: None,
            emissive_texture: None,
            emissive_uv_transform: None,
            normal_uv_transform: None,
            occlusion_uv_transform: None,
            roughness_texture: None,
            roughness_uv_transform: None,
            specular_factor: None,
            specular_color: None,
            ior: None,
            clearcoat_factor: None,
            clearcoat_roughness: None,
            sheen_color: None,
            sheen_roughness: None,
            transmission: None,
            volume_thickness: None,
            volume_attenuation_distance: None,
            iridescence_factor: None,
            iridescence_ior: None,
            anisotropy_strength: None,
            anisotropy_rotation: None,
            dispersion: None,
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
            palette: Vec::new(),
            indices: Vec::new(),
            mips: Vec::new(),
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
            uv_set2: None,
            emissive_texture: None,
            emissive_uv_transform: None,
            normal_uv_transform: None,
            occlusion_uv_transform: None,
            roughness_texture: None,
            roughness_uv_transform: None,
            specular_factor: None,
            specular_color: None,
            ior: None,
            clearcoat_factor: None,
            clearcoat_roughness: None,
            sheen_color: None,
            sheen_roughness: None,
            transmission: None,
            volume_thickness: None,
            volume_attenuation_distance: None,
            iridescence_factor: None,
            iridescence_ior: None,
            anisotropy_strength: None,
            anisotropy_rotation: None,
            dispersion: None,
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
                variant_materials: Vec::new(),
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
                uv_set2: None,
                emissive_texture: None,
                emissive_uv_transform: None,
                normal_uv_transform: None,
                occlusion_uv_transform: None,
                roughness_texture: None,
                roughness_uv_transform: None,
                specular_factor: None,
                specular_color: None,
                ior: None,
                clearcoat_factor: None,
                clearcoat_roughness: None,
                sheen_color: None,
                sheen_roughness: None,
                transmission: None,
                volume_thickness: None,
                volume_attenuation_distance: None,
                iridescence_factor: None,
                iridescence_ior: None,
                anisotropy_strength: None,
                anisotropy_rotation: None,
                dispersion: None,
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
            uv_set2: None,
            emissive_texture: None,
            emissive_uv_transform: None,
            normal_uv_transform: None,
            occlusion_uv_transform: None,
            roughness_texture: None,
            roughness_uv_transform: None,
            specular_factor: None,
            specular_color: None,
            ior: None,
            clearcoat_factor: None,
            clearcoat_roughness: None,
            sheen_color: None,
            sheen_roughness: None,
            transmission: None,
            volume_thickness: None,
            volume_attenuation_distance: None,
            iridescence_factor: None,
            iridescence_ior: None,
            anisotropy_strength: None,
            anisotropy_rotation: None,
            dispersion: None,
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
            uv_set2: None,
            emissive_texture: None,
            emissive_uv_transform: None,
            normal_uv_transform: None,
            occlusion_uv_transform: None,
            roughness_texture: None,
            roughness_uv_transform: None,
            specular_factor: None,
            specular_color: None,
            ior: None,
            clearcoat_factor: None,
            clearcoat_roughness: None,
            sheen_color: None,
            sheen_roughness: None,
            transmission: None,
            volume_thickness: None,
            volume_attenuation_distance: None,
            iridescence_factor: None,
            iridescence_ior: None,
            anisotropy_strength: None,
            anisotropy_rotation: None,
            dispersion: None,
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
fn secondary_maps_can_use_a_second_uv_set() {
    use mdc_model::{Material, Mesh, MeshGeometry, Model, Node, Texture};

    let png = |w: u32, h: u32, pixels: Vec<u8>| {
        let image = image::RgbImage::from_raw(w, h, pixels).unwrap();
        let mut out = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    };
    let build = |uv_set2: Option<u8>| {
        let mut geometry = MeshGeometry {
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
        };
        // UV set 1 mirrors the U axis.
        geometry.uv2 = geometry.uv.iter().map(|uv| [1.0 - uv[0], uv[1]]).collect();
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
                variant_materials: Vec::new(),
                ..Mesh::default()
            }],
            materials: vec![Material {
                name: "m".into(),
                texture: Some(0),
                color: Some([0, 0, 0]),
                emissive_texture: Some(0),
                emissive_strength: Some(1.0),
                uv_set: None,
                uv_set2,
                ..Material::default()
            }],
            // Left red, right green.
            textures: vec![Texture {
                name: "rg".into(),
                width: 2,
                height: 1,
                format: "image/png".into(),
                bytes: 0,
                data: Some(png(2, 1, vec![255, 0, 0, 0, 255, 0])),
                wrap: None,
            }],
            ..Model::default()
        };
        let blobs = mdc_bake::attach_model_textures(&model, &mdc_bake::BakeLimits::default());
        mdc_bake::mesh_package_with_textures(
            &model,
            &mdc_model::RendererCapability::full_3d(),
            blobs,
        )
        .unwrap()
    };
    let frame = |package: &mdc_model::MdcPackage| {
        mdc_bake::render_package_scene(&package.package, 0.0, (32, 32)).unwrap()
    };
    let left = |f: &mdc_bake::BakedTexture| {
        u16::from_le_bytes([f.rgb565[(16 * 32 + 6) * 2], f.rgb565[(16 * 32 + 6) * 2 + 1]])
    };
    // Without uv_set2 the emissive follows UV0 (red on the left).
    let same = left(&frame(&build(None)));
    // With uv_set2 = 1 the emissive samples the mirrored UV (green on the left).
    let secondary = left(&frame(&build(Some(1))));
    assert!(
        same >> 11 > ((same >> 5) & 0x3f),
        "uv0 left is red: {same:#06x}"
    );
    assert!(
        ((secondary >> 5) & 0x3f) > 20,
        "uv1 left adds green: {secondary:#06x}"
    );
}

#[test]
fn occlusion_is_sampled_per_pixel() {
    use mdc_model::{Material, Mesh, MeshGeometry, Model, Node, Texture};

    let png = |w: u32, h: u32, pixels: Vec<u8>| {
        let image = image::RgbImage::from_raw(w, h, pixels).unwrap();
        let mut out = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    };
    // A single triangle so a per-triangle centroid sample would be uniform.
    let model = Model {
        source: "test".into(),
        nodes: vec![Node {
            name: "root".into(),
            mesh: Some(0),
            ..Node::default()
        }],
        meshes: vec![Mesh {
            name: "tri".into(),
            vertices: 3,
            indices: 3,
            material: Some(0),
            geometry: Some(MeshGeometry {
                positions: vec![[-0.5, -0.5, 0.0], [0.5, -0.5, 0.0], [0.0, 0.5, 0.0]],
                indices: vec![0, 1, 2],
                uv: vec![[0.0, 0.0], [1.0, 0.0], [0.5, 1.0]],
                uv2: Vec::new(),
                normals: vec![[0.0, 0.0, 1.0]; 3],
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
            name: "ao".into(),
            color: Some([255, 255, 255]),
            occlusion_texture: Some(0),
            occlusion_strength: Some(1.0),
            ..Material::default()
        }],
        textures: vec![Texture {
            name: "ao".into(),
            width: 2,
            height: 1,
            format: "image/png".into(),
            bytes: 0,
            // Left black (occluded), right white.
            data: Some(png(2, 1, vec![0, 0, 0, 255, 255, 255])),
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
    let at = |x: usize, y: usize| {
        u16::from_le_bytes([
            frame.rgb565[(y * 32 + x) * 2],
            frame.rgb565[(y * 32 + x) * 2 + 1],
        ])
    };
    // Sample two points inside the triangle at different U coordinates.
    let left = at(10, 26);
    let right = at(24, 26);
    assert!(
        right > left,
        "per-pixel occlusion varies within one triangle: left={left:#06x} right={right:#06x}"
    );
}

#[test]
fn animated_scene_reaches_the_mirror() {
    use mdc_core::{ConnectionState, Session};
    use mdc_protocol::{SceneAnim, SceneNode};
    use mdc_simulator::{BoardProfile, FakeDevice};

    let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    // The scene is set through the session so the device rasterises it.
    session
        .set_scene(SceneNode {
            name: "fade".into(),
            kind: "rect".into(),
            rect: Some([0, 0, 800, 480]),
            value: Some("#FFFFFF".into()),
            value2: None,
            opacity: Some(255),
            visible: None,
            anim: Some(SceneAnim {
                duration_ms: 100,
                looping: false,
                opacity: vec![[0, 0], [100, 255]],
                rect: Vec::new(),
                focus: Vec::new(),
                easing: None,
            }),
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            children: Vec::new(),
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    session.start_mirror("main", "full-frame", 30).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();

    // N463: the mirrored frame follows the scene animation.
    device.clock.advance(100);
    device.emit_mirror_frame().unwrap();
    session.poll().unwrap();
    let end = session.take_mirror_frame().expect("frame");
    assert_eq!(end.bytes[0], 0xFF, "opaque at the end (white)");
    assert_eq!(end.bytes[1], 0xFF);

    device.clock.tick = 0;
    device.emit_mirror_frame().unwrap();
    session.poll().unwrap();
    let start = session.take_mirror_frame().expect("frame");
    assert_eq!(&start.bytes[0..2], &[0, 0], "transparent at t=0 (black)");
}

#[test]
fn full_pbr_material_renders_on_the_device() {
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
    let build = |pbr: bool| Model {
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
                tangents: vec![[1.0, 0.0, 0.0, 1.0]; 4],
                skin_joints: Vec::new(),
                skin_weights: Vec::new(),
                skin_joints2: Vec::new(),
                skin_weights2: Vec::new(),
                morph_targets: Vec::new(),
            }),
            ..Mesh::default()
        }],
        materials: vec![Material {
            name: "pbr".into(),
            texture: Some(0),
            color: Some([255, 255, 255]),
            roughness_texture: pbr.then_some(1),
            normal_texture: pbr.then_some(2),
            occlusion_texture: pbr.then_some(1),
            emissive_texture: pbr.then_some(3),
            emissive_strength: Some(0.6),
            metallic: Some(1.0),
            roughness: Some(0.3),
            ..Material::default()
        }],
        textures: vec![
            Texture {
                name: "base".into(),
                width: 1,
                height: 1,
                format: "image/png".into(),
                bytes: 0,
                data: Some(png(1, 1, vec![128, 128, 128])),
                wrap: None,
            },
            Texture {
                name: "pbr".into(),
                width: 2,
                height: 1,
                format: "image/png".into(),
                bytes: 0,
                data: Some(png(2, 1, vec![64, 64, 64, 200, 200, 200])),
                wrap: None,
            },
            Texture {
                name: "normal".into(),
                width: 1,
                height: 1,
                format: "image/png".into(),
                bytes: 0,
                data: Some(png(1, 1, vec![128, 128, 255])),
                wrap: None,
            },
            Texture {
                name: "emissive".into(),
                width: 1,
                height: 1,
                format: "image/png".into(),
                bytes: 0,
                data: Some(png(1, 1, vec![40, 0, 0])),
                wrap: None,
            },
        ],
        ..Model::default()
    };
    let render = |pbr: bool| -> Vec<u8> {
        let model = build(pbr);
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
    let plain = render(false);
    let pbr = render(true);
    assert_ne!(plain, pbr, "the full PBR material changes the frame");
}

#[test]
fn clearcoat_adds_a_second_highlight() {
    use mdc_model::{Material, Mesh, MeshGeometry, Model, Node};

    let build = |clearcoat: f32| Model {
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
            name: "coated".into(),
            color: Some([64, 64, 64]),
            metallic: Some(1.0),
            roughness: Some(0.6),
            clearcoat_factor: Some(clearcoat),
            clearcoat_roughness: Some(0.1),
            sheen_color: None,
            sheen_roughness: None,
            transmission: None,
            volume_thickness: None,
            volume_attenuation_distance: None,
            iridescence_factor: None,
            iridescence_ior: None,
            anisotropy_strength: None,
            anisotropy_rotation: None,
            dispersion: None,
            ..Material::default()
        }],
        ..Model::default()
    };
    let render = |clearcoat: f32| {
        let package = mdc_model::compile_to_package(
            &build(clearcoat),
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
    assert!(sum(&render(1.0)) > sum(&render(0.0)), "clearcoat brightens");
}

#[test]
fn indexed_texture_blob_renders_on_the_device() {
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
            texture: Some(0),
            color: Some([255, 255, 255]),
            ..Material::default()
        }],
        ..Model::default()
    };
    let mut package =
        mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::full_3d()).unwrap();
    // A red/green indexed texture (N415).
    let blob = mdc_model::TextureBlob {
        width: 2,
        height: 1,
        rgb565: vec![0x00, 0xF8, 0xE0, 0x07],
        alpha: Vec::new(),
        palette: Vec::new(),
        indices: Vec::new(),
        mips: Vec::new(),
    };
    let indexed = mdc_bake::index_texture_blob(&blob, 2);
    assert!(indexed.is_indexed());
    package.package.texture_blobs = vec![indexed.clone()];
    package.package.texture = Some(indexed);
    let frame = mdc_bake::render_package_scene(&package.package, 0.0, (32, 32)).unwrap();
    let at = |x: usize| {
        u16::from_le_bytes([
            frame.rgb565[(16 * 32 + x) * 2],
            frame.rgb565[(16 * 32 + x) * 2 + 1],
        ])
    };
    let left = at(6);
    let right = at(25);
    assert!(left != right, "the indexed texture paints two colours");
    assert!(left >> 11 > 8 || (right >> 5) & 0x3f > 8);
}

#[test]
fn sheen_adds_a_rim_highlight() {
    use mdc_model::{Material, Mesh, MeshGeometry, Model, Node};

    let build = |sheen: Option<[u8; 3]>| Model {
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
                // A grazing normal so the rim term is non-zero.
                normals: vec![[1.0, 0.0, 0.0]; 4],
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
            name: "cloth".into(),
            color: Some([64, 64, 64]),
            sheen_color: sheen,
            sheen_roughness: Some(0.2),
            transmission: None,
            volume_thickness: None,
            volume_attenuation_distance: None,
            iridescence_factor: None,
            iridescence_ior: None,
            anisotropy_strength: None,
            anisotropy_rotation: None,
            dispersion: None,
            ..Material::default()
        }],
        ..Model::default()
    };
    let render = |sheen: Option<[u8; 3]>| {
        let package =
            mdc_model::compile_to_package(&build(sheen), &mdc_model::RendererCapability::full_3d())
                .unwrap();
        mdc_bake::render_package_scene(&package.package, 0.0, (32, 32)).unwrap()
    };
    let sum = |f: &mdc_bake::BakedTexture| -> u64 {
        f.rgb565
            .chunks_exact(2)
            .map(|b| u64::from(u16::from_le_bytes([b[0], b[1]])) >> 5)
            .sum()
    };
    let on = sum(&render(Some([255, 255, 255])));
    let off = sum(&render(None));
    assert!(on > off, "sheen brightens");
}

#[test]
fn generated_tangents_enable_normal_mapping() {
    use mdc_model::{Material, Mesh, MeshGeometry, Model, Node, Texture};

    let png = |w: u32, h: u32, pixels: Vec<u8>| {
        let image = image::RgbImage::from_raw(w, h, pixels).unwrap();
        let mut out = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
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
                // N423: no authored tangents.
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
            normal_texture: normal,
            ..Material::default()
        }],
        textures: vec![Texture {
            name: "n".into(),
            width: 2,
            height: 1,
            format: "image/png".into(),
            bytes: 0,
            // Left tilts the normal away, right faces the light.
            data: Some(png(2, 1, vec![255, 0, 0, 128, 128, 255])),
            wrap: None,
        }],
        ..Model::default()
    };
    let render = |normal: Option<usize>| {
        let model = build(normal);
        let blobs = mdc_bake::attach_model_textures(&model, &mdc_bake::BakeLimits::default());
        let package = mdc_bake::mesh_package_with_textures(
            &model,
            &mdc_model::RendererCapability::full_3d(),
            blobs,
        )
        .unwrap();
        mdc_bake::render_package_scene(&package.package, 0.0, (32, 32)).unwrap()
    };
    let sum = |f: &mdc_bake::BakedTexture| -> u64 {
        f.rgb565
            .chunks_exact(2)
            .map(|b| u64::from(u16::from_le_bytes([b[0], b[1]])) >> 5)
            .sum()
    };
    // The normal map (with generated tangents) darkens the tilted triangle.
    assert!(
        sum(&render(Some(0))) < sum(&render(None)),
        "generated tangents apply the normal map"
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
fn mirror_websocket_pushes_device_frames() {
    // N500: `/ws` upgrades and then pushes one binary JPEG per mirrored frame.
    use mdc_core::{ConnectionState, Session};
    use mdc_simulator::{BoardProfile, FakeDevice};
    use std::io::{Read, Write};
    use std::time::Duration;

    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 8,
        height: 4,
        mirror: true,
        mirror_fps: 15,
        device_id: "sim-ws".into(),
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
        .write_all(
            b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
              Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
        )
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
    let text = String::from_utf8_lossy(&response);
    assert!(text.starts_with("HTTP/1.1 101"), "upgraded: {text:.40}");
    assert!(
        text.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="),
        "accept header"
    );
    // A binary message carrying a JPEG follows the handshake.
    let body = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .expect("handshake body");
    let payload = &response[body..];
    assert!(
        payload.windows(2).any(|w| w == [0xFF, 0xD8]),
        "a JPEG message arrived"
    );
    assert_eq!(http.ws_clients(), 1);
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
        palette: Vec::new(),
        indices: Vec::new(),
        mips: Vec::new(),
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

    let (link, mut device) = FakeDevice::pair(BoardProfile {
        width: 8,
        height: 8,
        renderer_mesh2d: true,
        ..BoardProfile::default()
    });
    let mut session = Session::new(link);
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
        visible: None,
        anim: None,
        rotate: None,
        blend: None,
        align: None,
        z: None,
        wrap: None,
        mode: None,
        focus: None,
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
        visible: None,
        anim: None,
        rotate: None,
        blend: None,
        align: None,
        z: None,
        wrap: None,
        mode: None,
        focus: None,
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
fn meshopt_compressed_model_bakes_and_installs() {
    // N533/N541: a meshopt-compressed glTF imports, compiles and installs.
    use mdc_core::{ConnectionState, Session};
    use mdc_simulator::{BoardProfile, FakeDevice};

    // The golden version-0 vertex fixture decodes to 4 x 12 bytes; interpreted as
    // VEC3 f32 positions it gives a usable (if tiny) mesh.
    let stream: Vec<u8> = vec![
        0xa0, 0x01, 0x3f, 0x00, 0x00, 0x00, 0x58, 0x57, 0x58, 0x01, 0x26, 0x00, 0x00, 0x00, 0x01,
        0x0c, 0x00, 0x00, 0x00, 0x58, 0x01, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
        0x3f, 0x00, 0x00, 0x00, 0x17, 0x18, 0x17, 0x01, 0x26, 0x00, 0x00, 0x00, 0x01, 0x0c, 0x00,
        0x00, 0x00, 0x17, 0x01, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    let mut json = String::from(
        r#"{"asset":{"version":"2.0"},
        "extensionsUsed":["EXT_meshopt_compression"],
        "extensionsRequired":["EXT_meshopt_compression"],
        "buffers":[{"byteLength":STREAM}],
        "bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":STREAM,
            "extensions":{"EXT_meshopt_compression":{"buffer":0,"byteOffset":0,"byteLength":STREAM,
                "byteStride":12,"count":4,"mode":"ATTRIBUTES"}}}],
        "accessors":[{"bufferView":0,"componentType":5126,"count":4,"type":"VEC3"}],
        "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
        "nodes":[{"name":"root","mesh":0}]}"#,
    );
    json = json.replace("STREAM", &stream.len().to_string());
    // GLB: JSON chunk then the compressed BIN chunk.
    let mut json_chunk = json.into_bytes();
    while !json_chunk.len().is_multiple_of(4) {
        json_chunk.push(0x20);
    }
    let mut bin_chunk = stream.clone();
    while !bin_chunk.len().is_multiple_of(4) {
        bin_chunk.push(0x00);
    }
    let total = 12 + 8 + json_chunk.len() + 8 + bin_chunk.len();
    let mut glb = Vec::new();
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&(total as u32).to_le_bytes());
    glb.extend_from_slice(&(json_chunk.len() as u32).to_le_bytes());
    glb.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
    glb.extend_from_slice(&json_chunk);
    glb.extend_from_slice(&(bin_chunk.len() as u32).to_le_bytes());
    glb.extend_from_slice(&0x004E_4942u32.to_le_bytes());
    glb.extend_from_slice(&bin_chunk);

    let model = mdc_model::GltfImporter
        .import(&glb, &mdc_model::ImportLimits::default())
        .unwrap();
    assert_eq!(
        model.meshes[0].geometry.as_ref().unwrap().positions.len(),
        4
    );
    let package =
        mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::full_3d()).unwrap();
    assert!(package.verify().valid);

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
}

#[test]
fn dirty_tiles_reconcile_with_the_device() {
    // N536: every tile the host sends is counted by the device, so a partial
    // update can be reconciled end to end.
    use mdc_core::{ConnectionState, DirtyTracker, Session};
    use mdc_simulator::{BoardProfile, FakeDevice};

    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 8,
        height: 8,
        supports_partial_update: true,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    // A full frame first, so the surface has a base frame to patch.
    session
        .send_frame(mdc_core::Frame {
            surface_id: "main".into(),
            width: 8,
            height: 8,
            bytes: vec![0u8; 8 * 8 * 2],
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    let frame = vec![0u8; 8 * 8 * 2];
    let mut tracker = DirtyTracker::new((8, 8), 4, 0.9);
    // Two separate regions -> two tiles.
    tracker.mark((0, 0, 1, 1));
    tracker.mark((4, 4, 1, 1));
    let base = device.current_frame_id();
    let decision = session
        .send_dirty_update("main", base, &frame, &mut tracker)
        .unwrap();
    assert_eq!(decision, mdc_core::UpdateDecision::Partial(2));
    device.poll().unwrap();
    session.poll().unwrap();
    let report =
        mdc_core::reconcile_tiles(&session.stats(), device.metrics_snapshot().tiles_received);
    assert!(report.consistent, "{report:?}");
    assert_eq!(report.sent, 2);
    assert_eq!(report.missing(), 0);
}

#[test]
fn vertex_budget_trim_installs_on_a_device() {
    // N524/N531: a trimmed package installs and reports the reduced draw budget.
    use mdc_core::{ConnectionState, Session};
    use mdc_model::{Material, Mesh, MeshGeometry, Model, Node};
    use mdc_simulator::{BoardProfile, FakeDevice};

    let quad = |name: &str| Mesh {
        name: name.into(),
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
            ..Default::default()
        }),
        ..Mesh::default()
    };
    let model = Model {
        source: "budget".into(),
        nodes: vec![
            Node {
                name: "root".into(),
                children: vec![1, 2],
                ..Node::default()
            },
            Node {
                name: "a".into(),
                mesh: Some(0),
                translation: [-0.5, 0.0, 0.0],
                ..Node::default()
            },
            Node {
                name: "b".into(),
                mesh: Some(1),
                translation: [0.5, 0.0, 0.0],
                ..Node::default()
            },
        ],
        meshes: vec![quad("a"), quad("b")],
        materials: vec![Material {
            name: "red".into(),
            texture: None,
            color: Some([255, 0, 0]),
            ..Default::default()
        }],
        ..Model::default()
    };
    let package =
        mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::full_3d()).unwrap();
    assert_eq!(mdc_bake::draw_vertices(&package.package), 8);
    // Trim to one quad's worth of vertices.
    let mut inner = package.package.clone();
    assert_eq!(mdc_bake::trim_to_vertex_budget(&mut inner, 4), 1);
    assert_eq!(mdc_bake::draw_vertices(&inner), 4);
    let package = mdc_model::MdcPackage::seal(inner).unwrap();
    assert!(package.verify().valid);

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
}

#[test]
fn gpu_instancing_imports_and_reaches_a_device() {
    // N513/N521: EXT_mesh_gpu_instancing expands into instance children that
    // compile and install on a device.
    use mdc_core::{ConnectionState, Session};
    use mdc_simulator::{BoardProfile, FakeDevice};

    // Accessor 0 declares 3 instances (no bufferView, so the transforms default
    // to identity); accessor 1 is the quad's POSITION.
    let gltf = br#"{"asset":{"version":"2.0"},
        "extensionsUsed":["EXT_mesh_gpu_instancing"],
        "nodes":[{"name":"tile","mesh":0,
            "extensions":{"EXT_mesh_gpu_instancing":{"attributes":{"TRANSLATION":0}}}}],
        "meshes":[{"primitives":[{"attributes":{"POSITION":1}}]}],
        "accessors":[{"count":3,"type":"VEC3"},{"count":4,"type":"VEC3"}]}"#;
    let model = mdc_model::GltfImporter
        .import(gltf, &mdc_model::ImportLimits::default())
        .unwrap();
    // One group plus three instances.
    assert_eq!(model.nodes.len(), 4);
    assert_eq!(model.nodes[0].mesh, None, "the group is mesh-less");
    assert_eq!(model.nodes[0].children.len(), 3);
    assert_eq!(
        model
            .nodes
            .iter()
            .filter(|node| node.mesh == Some(0))
            .count(),
        3,
        "each instance carries the mesh"
    );

    let package =
        mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::sprite_only())
            .unwrap();
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
}

#[test]
fn radial_gradient_renders_on_the_device() {
    // N515/N521: a radial gradient is brightest at the centre on the device.
    use mdc_core::{ConnectionState, Session};
    use mdc_protocol::SceneNode;
    use mdc_simulator::{BoardProfile, FakeDevice};

    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 9,
        height: 9,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    session
        .set_scene(SceneNode {
            name: "glow".into(),
            kind: "gradient".into(),
            rect: Some([0, 0, 9, 9]),
            value: Some("#FFFFFF".into()),
            value2: Some("#000000".into()),
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: Some("radial".into()),
            focus: None,
            children: Vec::new(),
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_scene());
    let centre = device.pixel_rgb565(4, 4).unwrap();
    let corner = device.pixel_rgb565(0, 0).unwrap();
    assert!(centre > 0xC000, "bright centre: {centre:#06x}");
    assert!(centre > corner, "centre brighter than corner");
}

#[test]
fn image_fit_per_side_padding_renders_on_the_device() {
    // N655/N661: a per-side pad insets the fitted image on the device.
    use mdc_core::{ConnectionState, Session};
    use mdc_protocol::SceneNode;
    use mdc_simulator::{BoardProfile, FakeDevice};

    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 5,
        height: 5,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    session
        .set_scene(SceneNode {
            name: "pic".into(),
            kind: "image".into(),
            rect: Some([0, 0, 5, 5]),
            value: Some("f800".into()),
            value2: Some("1,1".into()),
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: Some("fit,pad-l=2".into()),
            focus: None,
            children: Vec::new(),
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_scene());
    let mut lit = Vec::new();
    for y in 0..5u16 {
        for x in 0..5u16 {
            if device.pixel_rgb565(x, y).unwrap() != 0 {
                lit.push((x, y));
            }
        }
    }
    // Left pad 2 leaves a 3x5 box; the 3x3 square source is centred vertically.
    assert_eq!(lit.len(), 9, "{lit:?}");
    assert!(lit
        .iter()
        .all(|(x, y)| (2..=4).contains(x) && (1..=3).contains(y)));
}

#[test]
fn image_fit_padding_renders_on_the_device() {
    // N645/N651: `pad` insets a fitted image on the device.
    use mdc_core::{ConnectionState, Session};
    use mdc_protocol::SceneNode;
    use mdc_simulator::{BoardProfile, FakeDevice};

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
            name: "pic".into(),
            kind: "image".into(),
            rect: Some([0, 0, 4, 4]),
            value: Some("f800".into()),
            value2: Some("1,1".into()),
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: Some("fit,pad=1".into()),
            focus: None,
            children: Vec::new(),
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_scene());
    let mut lit = Vec::new();
    for y in 0..4u16 {
        for x in 0..4u16 {
            if device.pixel_rgb565(x, y).unwrap() != 0 {
                lit.push((x, y));
            }
        }
    }
    // A 1x1 source in a padded 2x2 inner box fills exactly those four pixels.
    assert_eq!(lit.len(), 4, "{lit:?}");
    assert!(lit
        .iter()
        .all(|(x, y)| (1..=2).contains(x) && (1..=2).contains(y)));
}

#[test]
fn image_fit_valign_renders_on_the_device() {
    // N635/N641: an `image` with `fit` and a vertical `align` letterboxes on the
    // device through the scene path.
    use mdc_core::{ConnectionState, Session};
    use mdc_protocol::SceneNode;
    use mdc_simulator::{BoardProfile, FakeDevice};

    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 1,
        height: 4,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    session
        .set_scene(SceneNode {
            name: "pic".into(),
            kind: "image".into(),
            rect: Some([0, 0, 1, 4]),
            value: Some("f800".into()),
            value2: Some("1,1".into()),
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: Some("top".into()),
            z: None,
            wrap: None,
            mode: Some("fit".into()),
            focus: None,
            children: Vec::new(),
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_scene());
    // The single-row letterbox lands at the top with `align: "top"`.
    assert_eq!(device.pixel_rgb565(0, 0).unwrap(), 0xF800);
    assert_eq!(device.pixel_rgb565(0, 1).unwrap(), 0);
    assert_eq!(device.pixel_rgb565(0, 3).unwrap(), 0);
}

#[test]
fn multi_stop_gradient_and_reserved_extension_diagnostics() {
    // N503/N505: a reserved extension is reported, and a multi-stop gradient
    // renders through each stop on the device.
    use mdc_core::{ConnectionState, Session};
    use mdc_protocol::SceneNode;
    use mdc_simulator::{BoardProfile, FakeDevice};

    // N503/N533: meshopt is detected and now decoded; draco is still rejected.
    let meshopt = br#"{"asset":{"version":"2.0"},
        "extensionsRequired":["EXT_meshopt_compression"]}"#;
    assert_eq!(
        mdc_model::reserved_extensions_in_bytes(meshopt),
        vec!["EXT_meshopt_compression".to_string()]
    );
    assert!(mdc_model::GltfImporter
        .import(meshopt, &mdc_model::ImportLimits::default())
        .is_ok());
    let draco = br#"{"asset":{"version":"2.0"},
        "extensionsRequired":["KHR_draco_mesh_compression"]}"#;
    assert!(mdc_model::GltfImporter
        .import(draco, &mdc_model::ImportLimits::default())
        .is_err());

    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 1,
        height: 11,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    session
        .set_scene(SceneNode {
            name: "ramp".into(),
            kind: "gradient".into(),
            rect: Some([0, 0, 1, 11]),
            value: Some("#FF0000".into()),
            value2: Some("0:#FF0000,50:#00FF00,100:#0000FF".into()),
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            children: Vec::new(),
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_scene());
    let green = device.pixel_rgb565(0, 5).unwrap();
    let red = device.pixel_rgb565(0, 0).unwrap();
    let blue = device.pixel_rgb565(0, 10).unwrap();
    assert!(((green >> 5) & 0x3f) > ((red >> 5) & 0x3f), "green middle");
    assert!((blue & 0x1f) > (red & 0x1f), "blue bottom");
}

#[test]
fn scene_align_z_wrap_and_dash_render_deterministically() {
    // N484/N485/N495/N497: the newer scene fields render together, stably.
    use mdc_core::{ConnectionState, Session};
    use mdc_protocol::SceneNode;
    use mdc_simulator::{BoardProfile, FakeDevice};

    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 24,
        height: 16,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

    let rect = |name: &str, color: &str, z: Option<i32>| SceneNode {
        name: name.into(),
        kind: "rect".into(),
        rect: Some([0, 0, 8, 8]),
        value: Some(color.into()),
        value2: None,
        opacity: None,
        visible: None,
        anim: None,
        rotate: None,
        blend: None,
        align: None,
        z,
        wrap: None,
        mode: None,
        focus: None,
        children: Vec::new(),
    };
    let scene = SceneNode {
        name: "root".into(),
        kind: "group".into(),
        rect: None,
        value: None,
        value2: None,
        opacity: None,
        visible: None,
        anim: None,
        rotate: None,
        blend: None,
        align: None,
        z: None,
        wrap: None,
        mode: None,
        focus: None,
        children: vec![
            // z lifts green above red despite the document order.
            rect("green", "#00FF00", Some(5)),
            rect("red", "#FF0000", None),
            SceneNode {
                name: "label".into(),
                kind: "text".into(),
                rect: Some([0, 8, 24, 7]),
                value: Some("hi".into()),
                value2: None,
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: Some("center".into()),
                z: None,
                wrap: Some("none".into()),
                mode: None,
                focus: None,
                children: Vec::new(),
            },
            SceneNode {
                name: "rule".into(),
                kind: "line".into(),
                rect: Some([0, 15, 23, 15]),
                value: Some("#FFFFFF".into()),
                value2: Some("4".into()),
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: None,
                focus: None,
                children: Vec::new(),
            },
        ],
    };
    session.set_scene(scene).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_scene());
    // z: green is on top of red.
    assert_eq!(device.pixel_rgb565(1, 1), Some(0x07E0));
    // The dashed rule alternates painted and empty pixels.
    let row: Vec<u16> = (0..24)
        .map(|x| device.pixel_rgb565(x, 15).unwrap())
        .collect();
    assert!(row.iter().any(|v| *v != 0) && row.contains(&0), "dashed");
    // The centred label drew on the text row.
    assert!((0..24).any(|x| device.pixel_rgb565(x, 8).unwrap() != 0));
    // Rendering is deterministic: a second pass matches the first.
    let first: Vec<Option<u16>> = (0..24 * 16)
        .map(|i| device.pixel_rgb565((i % 24) as u16, (i / 24) as u16))
        .collect();
    assert!(device.render_scene());
    let second: Vec<Option<u16>> = (0..24 * 16)
        .map(|i| device.pixel_rgb565((i % 24) as u16, (i / 24) as u16))
        .collect();
    assert_eq!(first, second, "rendering is deterministic");
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
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            value2: None,
        }],
        value: None,
        opacity: None,
        visible: None,
        anim: None,
        rotate: None,
        blend: None,
        align: None,
        z: None,
        wrap: None,
        mode: None,
        focus: None,
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

/// N551: assemble a GLB from a JSON chunk and a BIN chunk (4-byte aligned).
fn make_glb(json: &str, bin: &[u8]) -> Vec<u8> {
    let mut json_chunk = json.as_bytes().to_vec();
    while !json_chunk.len().is_multiple_of(4) {
        json_chunk.push(0x20);
    }
    let mut bin_chunk = bin.to_vec();
    while !bin_chunk.len().is_multiple_of(4) {
        bin_chunk.push(0x00);
    }
    let total = 12 + 8 + json_chunk.len() + 8 + bin_chunk.len();
    let mut glb = Vec::new();
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&(total as u32).to_le_bytes());
    glb.extend_from_slice(&(json_chunk.len() as u32).to_le_bytes());
    glb.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
    glb.extend_from_slice(&json_chunk);
    glb.extend_from_slice(&(bin_chunk.len() as u32).to_le_bytes());
    glb.extend_from_slice(&0x004E_4942u32.to_le_bytes());
    glb.extend_from_slice(&bin_chunk);
    glb
}

#[test]
fn meshopt_truncations_are_errors_not_panics() {
    // N543/N551: every prefix of a golden meshopt stream is a clean error.
    let stream: Vec<u8> = vec![
        0xa0, 0x01, 0x3f, 0x00, 0x00, 0x00, 0x58, 0x57, 0x58, 0x01, 0x26, 0x00, 0x00, 0x00, 0x01,
        0x0c, 0x00, 0x00, 0x00, 0x58, 0x01, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    for len in 0..stream.len() {
        assert!(mdc_model::decode_vertex_buffer(
            4,
            12,
            &stream[..len],
            mdc_model::MeshoptFilter::None
        )
        .is_err());
    }
    // An absurd declared count is refused before allocating.
    assert!(mdc_model::decode_vertex_buffer(
        usize::MAX / 8,
        8,
        &stream,
        mdc_model::MeshoptFilter::None
    )
    .is_err());
}

#[test]
fn ktx2_texture_reports_metadata_without_transcoding() {
    // N544/N551: a Basis/KTX2 texture imports with its real size, is flagged by
    // validation, and yields no decoded blob (transcoding is refused).
    let mut ktx2 = mdc_model::KTX2_IDENTIFIER.to_vec();
    let mut field = |value: u32| ktx2.extend_from_slice(&value.to_le_bytes());
    field(0); // vkFormat: undefined
    field(0);
    field(256);
    field(128);
    field(0);
    field(0);
    field(1);
    field(1);
    field(1); // BasisLZ
    for _ in 0..4 {
        field(0);
    }
    ktx2.extend_from_slice(&0u64.to_le_bytes());
    ktx2.extend_from_slice(&0u64.to_le_bytes());
    assert_eq!(ktx2.len(), mdc_model::KTX2_HEADER_BYTES);

    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},
        "extensionsUsed":["KHR_texture_basisu"],
        "buffers":[{{"byteLength":{len}}}],
        "bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":{len}}}],
        "images":[{{"bufferView":0,"width":1,"height":1}}],
        "textures":[{{"source":0}}],
        "materials":[{{"pbrMetallicRoughness":{{"baseColorTexture":{{"index":0}}}}}}]}}"#,
        len = ktx2.len()
    );
    let glb = make_glb(&json, &ktx2);
    let model = mdc_model::GltfImporter
        .import(&glb, &mdc_model::ImportLimits::default())
        .unwrap();
    assert_eq!(model.textures[0].format, "image/ktx2");
    assert_eq!(
        (model.textures[0].width, model.textures[0].height),
        (256, 128)
    );
    assert_eq!(model.summary().ktx2_textures, 1);
    assert!(model.validate().warnings.iter().any(|w| w.contains("KTX2")));
    // The bake refuses to transcode: every texture blob is empty.
    let blobs = mdc_bake::attach_model_textures(&model, &mdc_bake::BakeLimits::default());
    assert!(blobs.iter().all(|blob| !blob.validate()));
}

#[test]
fn focus_animation_changes_the_device_frame() {
    // N546/N551: a focus keyframe track moves the radial centre on the device.
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 9,
        height: 9,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    let node = mdc_protocol::SceneNode {
        name: "spot".into(),
        kind: "gradient".into(),
        rect: Some([0, 0, 9, 9]),
        children: Vec::new(),
        value: Some("#FFFFFF".into()),
        value2: Some("#000000".into()),
        opacity: None,
        visible: None,
        anim: Some(mdc_protocol::SceneAnim {
            duration_ms: 100,
            looping: false,
            opacity: Vec::new(),
            rect: Vec::new(),
            focus: vec![[0, 0, 0], [100, 100, 100]],
            easing: None,
        }),
        rotate: None,
        blend: None,
        align: None,
        z: None,
        wrap: None,
        mode: Some("radial".into()),
        focus: Some("0,0".into()),
    };
    session.set_scene(node).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    device.clock.tick = 0;
    assert!(device.render_scene());
    let start = device.pixel_rgb565(0, 0).unwrap();
    device.clock.tick = 100;
    assert!(device.render_scene());
    let end = device.pixel_rgb565(0, 0).unwrap();
    assert_ne!(start, end, "the focus track moved the bright centre");
}

#[test]
fn mirror_websocket_reassembles_client_fragments() {
    // N548/N551: a fragmented client -> server WebSocket message is reassembled
    // and counted, without disturbing control frames.
    use std::io::{Read, Write};
    fn client_frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mask = [0x01u8, 0x02, 0x03, 0x04];
        let mut out = vec![
            if fin { 0x80 } else { 0x00 } | opcode,
            0x80 | payload.len() as u8,
        ];
        out.extend_from_slice(&mask);
        out.extend(
            payload
                .iter()
                .enumerate()
                .map(|(i, byte)| byte ^ mask[i % 4]),
        );
        out
    }
    let mut server = mdc_mirror::HttpMirrorServer::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();
    let mut client = std::net::TcpStream::connect(addr).unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_millis(20)))
        .unwrap();
    client
        .write_all(
            b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
              Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
        )
        .unwrap();
    let mut seen = Vec::new();
    for _ in 0..100 {
        let _ = server.pump();
        let mut buffer = [0u8; 4096];
        match client.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => seen.extend_from_slice(&buffer[..n]),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(2)),
        }
        if seen.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    client.write_all(&client_frame(false, 0x1, b"po")).unwrap();
    client.write_all(&client_frame(true, 0x0, b"ng")).unwrap();
    for _ in 0..100 {
        let _ = server.pump();
        if server.ws_messages() == 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(server.ws_messages(), 1);
}

#[test]
fn device_tiles_snapshot_reconciles_end_to_end() {
    // N547/N551: the device reports applied tiles over diagnostics, and the host
    // snapshot folds them into one reconciliation.
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 8,
        height: 8,
        supports_partial_update: true,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 8,
            height: 8,
            bytes: vec![0u8; 8 * 8 * 2],
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    let frame = vec![0u8; 8 * 8 * 2];
    let mut tracker = mdc_core::DirtyTracker::new((8, 8), 4, 0.9);
    tracker.mark((0, 0, 1, 1));
    let base = device.current_frame_id();
    session
        .send_dirty_update("main", base, &frame, &mut tracker)
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    // The device reports what it applied; the host snapshot reconciles it.
    device.emit_tiles_received().unwrap();
    session.poll().unwrap();
    assert_eq!(
        session.stats().device_tiles_received,
        device.metrics_snapshot().tiles_received
    );
    let report = session.tile_reconciliation();
    assert!(report.consistent, "{report:?}");
    assert_eq!(session.stats().tiles_missing, 0);
}

#[test]
fn compacted_geometry_installs_and_keeps_the_triangles() {
    // N545/N551: lossless geometry compaction preserves the triangle set and the
    // package still installs on a device.
    let mut geometry = mdc_model::MeshGeometry {
        positions: vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
        ],
        indices: vec![0, 1, 2, 3, 4, 5],
        ..Default::default()
    };
    let before = geometry.positions.len();
    assert_eq!(mdc_bake::compact_geometry(&mut geometry), 3);
    assert_eq!(geometry.positions.len(), 3);
    assert_eq!(geometry.indices, vec![0, 1, 2, 0, 1, 2]);
    assert!(geometry.positions.len() < before);

    let model = mdc_model::Model {
        source: "test".into(),
        meshes: vec![mdc_model::Mesh {
            vertices: 3,
            indices: 6,
            geometry: Some(geometry),
            ..Default::default()
        }],
        nodes: vec![mdc_model::Node {
            name: "root".into(),
            mesh: Some(0),
            ..Default::default()
        }],
        ..Default::default()
    };
    let package =
        mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::full_3d()).unwrap();
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
}

#[test]
fn ktx2_array_and_cube_containers_import_without_transcoding() {
    // N554/N561: KTX2 layer/face/depth counts are read; a cubemap Basis texture
    // imports as metadata and is flagged rather than transcode-attempted.
    let mut ktx2 = mdc_model::KTX2_IDENTIFIER.to_vec();
    let mut field = |value: u32| ktx2.extend_from_slice(&value.to_le_bytes());
    field(0); // vkFormat undefined
    field(0);
    field(64);
    field(64);
    field(0);
    field(2); // layerCount (array)
    field(6); // faceCount (cubemap)
    field(3); // levelCount
    field(1); // BasisLZ
    for _ in 0..4 {
        field(0);
    }
    ktx2.extend_from_slice(&0u64.to_le_bytes());
    ktx2.extend_from_slice(&0u64.to_le_bytes());
    let header = mdc_model::Ktx2Header::parse(&ktx2).expect("header");
    assert!(header.is_cubemap() && header.is_array());
    assert_eq!(header.mip_levels(), 3);
    assert_eq!(header.slices(), 12);

    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},
        "extensionsUsed":["KHR_texture_basisu"],
        "buffers":[{{"byteLength":{len}}}],
        "bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":{len}}}],
        "images":[{{"bufferView":0,"width":64,"height":64}}],
        "textures":[{{"source":0}}],
        "materials":[{{"pbrMetallicRoughness":{{"baseColorTexture":{{"index":0}}}}}}]}}"#,
        len = ktx2.len()
    );
    let glb = make_glb(&json, &ktx2);
    let model = mdc_model::GltfImporter
        .import(&glb, &mdc_model::ImportLimits::default())
        .unwrap();
    assert!(model.textures[0].is_ktx2());
    assert_eq!(model.summary().ktx2_textures, 1);
    // N570: two layers × six faces = twelve slices survive to the IR summary.
    assert_eq!(model.summary().ktx2_slices, 12);
    assert_eq!(model.textures[0].ktx2_header().unwrap().slices(), 12);
}

#[test]
fn optimized_index_order_installs_on_a_device() {
    // N555/N561: reordering triangles for cache locality keeps the triangle set
    // and still installs.
    let mut geometry = mdc_model::MeshGeometry {
        positions: (0..6).map(|i| [i as f32, 0.0, 0.0]).collect(),
        indices: vec![2, 3, 4, 0, 1, 2, 4, 3, 5, 2, 1, 3],
        ..Default::default()
    };
    let before = mdc_bake::index_cache_misses(&geometry, 4);
    let after = mdc_bake::optimize_index_order(&mut geometry, 4);
    assert!(after < before, "{before} -> {after}");
    let model = mdc_model::Model {
        source: "test".into(),
        meshes: vec![mdc_model::Mesh {
            vertices: 6,
            indices: geometry.indices.len() as u32,
            geometry: Some(geometry),
            ..Default::default()
        }],
        nodes: vec![mdc_model::Node {
            name: "root".into(),
            mesh: Some(0),
            ..Default::default()
        }],
        ..Default::default()
    };
    let package =
        mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::full_3d()).unwrap();
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
}

#[test]
fn image_node_scales_on_the_device() {
    // N556/N561: a scene `image` with `value2` source dims and `mode: linear`
    // scales a small source into the rect.
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 4,
        height: 1,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    session
        .set_scene(mdc_protocol::SceneNode {
            name: "pic".into(),
            kind: "image".into(),
            rect: Some([0, 0, 4, 1]),
            children: Vec::new(),
            value: Some("f800001f".into()),
            value2: Some("2,1".into()),
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: Some("linear".into()),
            focus: None,
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_scene());
    assert_eq!(device.pixel_rgb565(0, 0).unwrap(), 0xF800);
    assert_eq!(device.pixel_rgb565(3, 0).unwrap(), 0x001F);
    let interior = device.pixel_rgb565(1, 0).unwrap();
    assert_ne!(interior, 0xF800);
    assert_ne!(interior, 0x001F);
}

#[test]
fn tile_reconciliation_threshold_requests_a_full_frame() {
    // N557/N561: unacknowledged tiles past a threshold raise the full-frame flag.
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 8,
        height: 8,
        supports_partial_update: true,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 8,
            height: 8,
            bytes: vec![0u8; 8 * 8 * 2],
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    let frame = vec![0u8; 8 * 8 * 2];
    let mut tracker = mdc_core::DirtyTracker::new((8, 8), 4, 0.9);
    tracker.mark((0, 0, 1, 1));
    tracker.mark((4, 4, 1, 1));
    let base = device.current_frame_id();
    session
        .send_dirty_update("main", base, &frame, &mut tracker)
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    // The device acknowledged nothing; a tight threshold forces a full frame.
    let report = session.enforce_tile_reconciliation(0);
    assert!(report.needs_full_frame(0), "{report:?}");
    assert!(session.needs_full_frame);
}

#[test]
fn mirror_websocket_ignores_reserved_compression_bit_end_to_end() {
    // N558/N561: a reserved-bit frame is counted, not treated as a message.
    use std::io::{Read, Write};
    let mut server = mdc_mirror::HttpMirrorServer::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();
    let mut client = std::net::TcpStream::connect(addr).unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_millis(20)))
        .unwrap();
    client
        .write_all(
            b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
              Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
        )
        .unwrap();
    let mut seen = Vec::new();
    for _ in 0..100 {
        let _ = server.pump();
        let mut buffer = [0u8; 4096];
        match client.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => seen.extend_from_slice(&buffer[..n]),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(2)),
        }
        if seen.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let mask = [0x11u8, 0x22, 0x33, 0x44];
    let payload = b"ok";
    let mut frame = vec![0x80 | 0x40 | 0x02, 0x80 | payload.len() as u8];
    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(i, byte)| byte ^ mask[i % 4]),
    );
    client.write_all(&frame).unwrap();
    for _ in 0..100 {
        let _ = server.pump();
        if server.ws_reserved() == 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(server.ws_reserved(), 1);
    assert_eq!(server.ws_messages(), 0);
}

#[test]
fn tile_threshold_clears_after_a_full_frame() {
    // N566/N571: once a threshold raises the full-frame barrier, a full frame
    // clears it and the host converges back to tiling.
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 8,
        height: 8,
        supports_partial_update: true,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 8,
            height: 8,
            bytes: vec![0u8; 8 * 8 * 2],
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    let frame = vec![0u8; 8 * 8 * 2];
    let mut tracker = mdc_core::DirtyTracker::new((8, 8), 4, 0.9);
    tracker.mark((0, 0, 1, 1));
    tracker.mark((4, 4, 1, 1));
    let base = device.current_frame_id();
    session
        .send_dirty_update("main", base, &frame, &mut tracker)
        .unwrap();
    // The device never polls, so nothing was acknowledged: a tight threshold
    // raises the barrier.
    session.enforce_tile_reconciliation(0);
    assert!(session.needs_full_frame);
    // A full frame satisfies the barrier.
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 8,
            height: 8,
            bytes: vec![1u8; 8 * 8 * 2],
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(!session.needs_full_frame, "converged after a full frame");
    // Tiling is accepted again.
    let mut tracker = mdc_core::DirtyTracker::new((8, 8), 4, 0.9);
    tracker.mark((0, 0, 1, 1));
    let base = device.current_frame_id();
    assert!(session
        .send_dirty_update("main", base, &frame, &mut tracker)
        .is_ok());
}

#[test]
fn image_node_flip_and_rotate_on_the_device() {
    // N565/N571: a scene image flipped/rotated on the device swaps its halves.
    let render = |mode: &str, rotate: u16| {
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 1,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .set_scene(mdc_protocol::SceneNode {
                name: "pic".into(),
                kind: "image".into(),
                rect: Some([0, 0, 4, 1]),
                children: Vec::new(),
                value: Some("f800001f".into()),
                value2: Some("2,1".into()),
                opacity: None,
                visible: None,
                anim: None,
                rotate: Some(rotate),
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: Some(mode.into()),
                focus: None,
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_scene());
        device.pixel_rgb565(0, 0).unwrap()
    };
    // Baseline left column is red; flip and 180° make it blue.
    assert_eq!(render("", 0), 0xF800);
    assert_eq!(render("flip-h", 0), 0x001F);
    assert_eq!(render("", 180), 0x001F);
}

#[test]
fn mirror_websocket_rejects_reserved_bit_with_close_1002() {
    // N567/N571: with the option on, a compressed-bit frame closes with 1002.
    use std::io::{Read, Write};
    let mut server = mdc_mirror::HttpMirrorServer::bind("127.0.0.1:0")
        .unwrap()
        .with_ws_reject_reserved(true);
    let addr = server.local_addr().unwrap();
    let mut client = std::net::TcpStream::connect(addr).unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_millis(20)))
        .unwrap();
    client
        .write_all(
            b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
              Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
              Sec-WebSocket-Extensions: permessage-deflate\r\n\r\n",
        )
        .unwrap();
    let mut handshake = Vec::new();
    for _ in 0..100 {
        let _ = server.pump();
        let mut buffer = [0u8; 4096];
        match client.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                handshake.extend_from_slice(&buffer[..n]);
                if handshake.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(2)),
        }
    }
    assert_eq!(server.ws_offered(), 1);
    assert!(!String::from_utf8_lossy(&handshake)
        .to_ascii_lowercase()
        .contains("permessage-deflate"));
    let mask = [0xAAu8, 0xBB, 0xCC, 0xDD];
    let payload = b"z";
    let mut frame = vec![0x80 | 0x40 | 0x02, 0x80 | payload.len() as u8];
    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(i, byte)| byte ^ mask[i % 4]),
    );
    client.write_all(&frame).unwrap();
    let mut close = Vec::new();
    for _ in 0..100 {
        let _ = server.pump();
        let mut buffer = [0u8; 64];
        match client.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => close.extend_from_slice(&buffer[..n]),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(2)),
        }
        if close.len() >= 4 {
            break;
        }
    }
    assert_eq!(close[0], 0x88);
    assert_eq!(u16::from_be_bytes([close[2], close[3]]), 1002);
    // N577/N581/N607: the decline/failure reasons are logged, structured.
    assert!(server
        .ws_log()
        .iter()
        .any(|note| note.kind == "extension" && note.detail == "permessage-deflate"));
    assert!(server
        .ws_log()
        .iter()
        .any(|note| note.kind == "reserved" && note.detail == "closed 1002"));
}

#[test]
fn image_fit_letterboxes_on_the_device() {
    // N575/N581: `mode: fit` preserves the source aspect ratio.
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 4,
        height: 1,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    session
        .set_scene(mdc_protocol::SceneNode {
            name: "pic".into(),
            kind: "image".into(),
            rect: Some([0, 0, 4, 1]),
            children: Vec::new(),
            value: Some("f800".into()),
            value2: Some("1,1".into()),
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: Some("fit".into()),
            focus: None,
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_scene());
    // Only the centre column of the 4x1 rect is drawn.
    assert_eq!(device.pixel_rgb565(0, 0).unwrap(), 0);
    assert_eq!(device.pixel_rgb565(1, 0).unwrap(), 0xF800);
    assert_eq!(device.pixel_rgb565(2, 0).unwrap(), 0);
    assert_eq!(device.pixel_rgb565(3, 0).unwrap(), 0);
}

#[test]
fn tile_missing_threshold_is_negotiated() {
    // N586/N591: the host reads the device's tolerated-miss threshold from config
    // and uses it to decide when to force a full frame.
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 8,
        height: 8,
        supports_partial_update: true,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    session.get_config("tile_missing_threshold").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(session.tile_missing_threshold(), Some(0));
    session.set_config("tile_missing_threshold", "5").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(session.tile_missing_threshold(), Some(5));

    // Two unacknowledged tiles are fine under 5, but not under 0.
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 8,
            height: 8,
            bytes: vec![0u8; 8 * 8 * 2],
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    let base = device.current_frame_id();
    let frame = vec![0u8; 8 * 8 * 2];
    let mut tracker = mdc_core::DirtyTracker::new((8, 8), 4, 0.9);
    tracker.mark((0, 0, 1, 1));
    tracker.mark((4, 4, 1, 1));
    session
        .send_dirty_update("main", base, &frame, &mut tracker)
        .unwrap();
    let report = session.enforce_tile_reconciliation_from_config();
    assert_eq!(report.missing(), 2);
    assert!(!session.needs_full_frame, "under the negotiated threshold");
    session.set_config("tile_missing_threshold", "0").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    session.enforce_tile_reconciliation_from_config();
    assert!(session.needs_full_frame, "at the negotiated threshold");
}

#[test]
fn image_fit_alignment_on_the_device() {
    // N585/N591: `fit` honours `align` for the letterbox position.
    let render = |align: &str| {
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 1,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .set_scene(mdc_protocol::SceneNode {
                name: "pic".into(),
                kind: "image".into(),
                rect: Some([0, 0, 4, 1]),
                children: Vec::new(),
                value: Some("f800".into()),
                value2: Some("1,1".into()),
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: Some(align.into()),
                z: None,
                wrap: None,
                mode: Some("fit".into()),
                focus: None,
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_scene());
        (0..4)
            .map(|x| device.pixel_rgb565(x, 0).unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(render("left"), vec![0xF800, 0, 0, 0]);
    assert_eq!(render("center"), vec![0, 0xF800, 0, 0]);
    assert_eq!(render("right"), vec![0, 0, 0, 0xF800]);
}

#[test]
fn image_fit_composes_with_transforms_on_the_device() {
    // N595/N601: `fit` combined with `flip-h` keeps the letterbox and swaps halves.
    let render = |mode: &str| {
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 1,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .set_scene(mdc_protocol::SceneNode {
                name: "pic".into(),
                kind: "image".into(),
                rect: Some([0, 0, 4, 1]),
                children: Vec::new(),
                value: Some("f800001f".into()),
                value2: Some("2,1".into()),
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: Some(mode.into()),
                focus: None,
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_scene());
        (0..4)
            .map(|x| device.pixel_rgb565(x, 0).unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(render("fit"), vec![0, 0xF800, 0x001F, 0]);
    assert_eq!(render("fit,flip-h"), vec![0, 0x001F, 0xF800, 0]);
}

#[test]
fn board_tile_threshold_default_is_negotiated() {
    // N596/N601: a board ships its own default for the threshold config key.
    let read = |board: &str| {
        let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id(board));
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .get_config(mdc_core::TILES_MISSING_THRESHOLD_CONFIG)
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .config(mdc_core::TILES_MISSING_THRESHOLD_CONFIG)
            .map(str::to_string)
    };
    assert_eq!(read("default").as_deref(), Some("0"));
    assert_eq!(read("linux-virt").as_deref(), Some("2"));
}

#[test]
fn image_fit_quarter_turn_on_the_device() {
    // N605/N611: a 90° turn swaps the fitted box dimensions.
    let render = |rotate: u16| {
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 4,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .set_scene(mdc_protocol::SceneNode {
                name: "pic".into(),
                kind: "image".into(),
                rect: Some([0, 0, 4, 4]),
                children: Vec::new(),
                value: Some("f800001f".into()),
                value2: Some("1,2".into()),
                opacity: None,
                visible: None,
                anim: None,
                rotate: Some(rotate),
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: Some("fit".into()),
                focus: None,
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_scene());
        (
            device.pixel_rgb565(0, 0).unwrap(),
            device.pixel_rgb565(1, 0).unwrap(),
            device.pixel_rgb565(0, 1).unwrap(),
        )
    };
    let (o0, c10, _r01) = render(0);
    assert_eq!(o0, 0, "top-left outside the tall box");
    assert_ne!(c10, 0, "inside the tall box");
    let (o0, c10, r01) = render(90);
    assert_eq!(o0, 0, "top-left outside the wide box");
    assert_eq!(c10, 0, "top row outside the wide box");
    assert_ne!(r01, 0, "second row inside the wide box");
}

#[test]
fn board_advertises_its_config_default() {
    // N616/N621: the board's factory default rides in Capabilities.config.defaults,
    // so the host can read it without a get_config round trip.
    let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(
        session.advertised_config_default(mdc_core::TILES_MISSING_THRESHOLD_CONFIG),
        Some("2")
    );
    // Without `get_config`, the advertised default is still usable.
    assert_eq!(session.tile_missing_threshold(), Some(2));
    // The baseline board advertises no override.
    let (host, mut device) = FakeDevice::pair(BoardProfile::default());
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(
        session.advertised_config_default(mdc_core::TILES_MISSING_THRESHOLD_CONFIG),
        None
    );
}

#[test]
fn image_fit_align_with_rotate90_on_the_device() {
    // N625/N631: `align` positions the rotated letterbox.
    let render = |align: &str| {
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 8,
            height: 4,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .set_scene(mdc_protocol::SceneNode {
                name: "pic".into(),
                kind: "image".into(),
                rect: Some([0, 0, 8, 4]),
                children: Vec::new(),
                value: Some("f80007e0001ffffff81f07ff".into()),
                value2: Some("2,3".into()),
                opacity: None,
                visible: None,
                anim: None,
                rotate: Some(90),
                blend: None,
                align: Some(align.into()),
                z: None,
                wrap: None,
                mode: Some("fit".into()),
                focus: None,
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_scene());
        (0..8).find(|&x| (0..4).any(|y| device.pixel_rgb565(x, y).unwrap() != 0))
    };
    assert_eq!(render("left"), Some(0));
    assert_eq!(render("center"), Some(1));
    assert_eq!(render("right"), Some(2));
}

#[test]
fn image_fit_pad_composes_with_rotate_on_the_device() {
    // N665/N670: padding insets the box, then the quarter-turn fits inside it.
    let lit = |rotate: u16| {
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 4,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .set_scene(mdc_protocol::SceneNode {
                name: "pic".into(),
                kind: "image".into(),
                rect: Some([0, 0, 4, 4]),
                children: Vec::new(),
                value: Some("f800001f".into()),
                value2: Some("2,1".into()),
                opacity: None,
                visible: None,
                anim: None,
                rotate: Some(rotate),
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: Some("fit,pad=1".into()),
                focus: None,
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_scene());
        let mut pixels = Vec::new();
        for y in 0..4u16 {
            for x in 0..4u16 {
                if device.pixel_rgb565(x, y).unwrap() != 0 {
                    pixels.push((x, y));
                }
            }
        }
        pixels
    };
    let flat = lit(0);
    assert_eq!(flat.len(), 2);
    assert!(flat.iter().all(|(_, y)| *y == 1));
    let turned = lit(90);
    assert_eq!(turned.len(), 2);
    assert!(turned.iter().all(|(x, _)| *x == 1));
}

#[test]
fn sprite_fit_and_pad_on_the_device() {
    // N685/N691: a sprite honours fit/pad like an image.
    let lit = |mode: &str| {
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 4,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
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
            estimated_bytes: 8,
            atlas: Some(mdc_model::AtlasBlob {
                sprite_width: 1,
                sprite_height: 1,
                frames: 1,
                columns: 1,
                rows: 1,
                rgb565: vec![0x00, 0xF8],
                clips: Vec::new(),
                mips: Vec::new(),
            }),
            texture: None,
            texture_blobs: Vec::new(),
        };
        let sealed = mdc_model::MdcPackage::seal(package).unwrap();
        session
            .install_package(&sealed.to_bytes().unwrap())
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .set_scene(mdc_protocol::SceneNode {
                name: "icon".into(),
                kind: "sprite".into(),
                rect: Some([0, 0, 4, 4]),
                children: Vec::new(),
                value: Some("0".into()),
                value2: None,
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: Some(mode.into()),
                focus: None,
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_scene());
        let mut pixels = Vec::new();
        for y in 0..4u16 {
            for x in 0..4u16 {
                if device.pixel_rgb565(x, y).unwrap() != 0 {
                    pixels.push((x, y));
                }
            }
        }
        pixels
    };
    assert_eq!(lit("").len(), 16);
    let padded = lit("fit,pad=1");
    assert_eq!(padded.len(), 4, "{padded:?}");
    assert!(padded
        .iter()
        .all(|(x, y)| (1..=2).contains(x) && (1..=2).contains(y)));
}

#[test]
fn sprite_linear_filter_reaches_the_device() {
    // N705/N711: a sprite's `linear` token is honoured on the device double, so
    // an upscaled atlas frame blends instead of repeating the nearest texel.
    let render = |mode: Option<&str>| {
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 1,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
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
            estimated_bytes: 8,
            atlas: Some(mdc_model::AtlasBlob {
                sprite_width: 2,
                sprite_height: 1,
                frames: 1,
                columns: 1,
                rows: 1,
                rgb565: vec![0x00, 0xF8, 0x1F, 0x00],
                clips: Vec::new(),
                mips: Vec::new(),
            }),
            texture: None,
            texture_blobs: Vec::new(),
        };
        let sealed = mdc_model::MdcPackage::seal(package).unwrap();
        session
            .install_package(&sealed.to_bytes().unwrap())
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .set_scene(mdc_protocol::SceneNode {
                name: "icon".into(),
                kind: "sprite".into(),
                rect: Some([0, 0, 4, 1]),
                children: Vec::new(),
                value: Some("0".into()),
                value2: None,
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: mode.map(str::to_string),
                focus: None,
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_scene());
        [0, 1, 2, 3].map(|x| device.pixel_rgb565(x, 0).unwrap())
    };
    assert_eq!(render(None), [0xF800, 0xF800, 0x001F, 0x001F]);
    let linear = render(Some("linear"));
    assert_eq!(linear[0], 0xF800);
    assert_eq!(linear[3], 0x001F);
    assert_ne!(linear[1], 0xF800);
    assert_ne!(linear[1], 0x001F);
}

#[test]
fn package_order_summary_survives_a_round_trip() {
    // N704/N711: the persisted before/after locality summary is metadata only;
    // the container still verifies and the fields survive a re-open.
    let doc = br#"{"asset":{"version":"2.0"},
        "nodes":[{"name":"root","mesh":0}],
        "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
        "accessors":[{"count":4,"type":"VEC3"}]}"#;
    let model = mdc_model::GltfImporter
        .import(doc, &mdc_model::ImportLimits::default())
        .unwrap();
    let mut package =
        mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::sprite_only())
            .unwrap();
    let before = package.manifest.cache_misses + 9;
    package.set_cache_report(before);
    let back = mdc_model::MdcPackage::from_bytes(&package.to_bytes().unwrap()).unwrap();
    assert_eq!(back.manifest.cache_misses_before, before);
    assert_eq!(back.manifest.cache_misses_saved, 9);
    let report = back.verify();
    assert!(report.valid);
    assert_eq!(report.manifest_cache_misses_before, before);
    assert_eq!(report.manifest_cache_misses_saved, 9);
    assert_eq!(back.package.ktx2_summary().textures, 0);
    assert_eq!(back.package.ktx2_summary().slices, 0);
}

#[test]
fn health_kind_filter_reaches_an_http_client() {
    // N717/N721: `/health?kind=` narrows the structured entries while the
    // counters stay global; an unknown kind returns an empty entry list.
    let fetch = |kind: &str| {
        let mut server = mdc_mirror::HttpMirrorServer::bind("127.0.0.1:0").unwrap();
        server.note_ws("reserved", "r1");
        server.note_ws("extension", "e1");
        let addr = server.local_addr().unwrap();
        let mut client = std::net::TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_millis(20)))
            .unwrap();
        client
            .write_all(format!("GET /health?kind={kind} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
            .unwrap();
        let mut seen = Vec::new();
        for _ in 0..100 {
            let _ = server.pump();
            let mut buffer = [0u8; 4096];
            match client.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => seen.extend_from_slice(&buffer[..n]),
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(2)),
            }
            if seen.windows(4).any(|w| w == b"\r\n\r\n") && seen.len() > 200 {
                break;
            }
        }
        String::from_utf8_lossy(&seen).into_owned()
    };
    let reserved = fetch("reserved");
    assert!(
        reserved.contains("\"ws_log_entries\":[{\"kind\":\"reserved\",\"detail\":\"r1\"}]"),
        "{reserved}"
    );
    assert!(!reserved.contains("\"kind\":\"extension\""), "{reserved}");
    assert!(reserved.contains("\"ws_log\":2"), "{reserved}");
    let missing = fetch("no-such-kind");
    assert!(missing.contains("\"ws_log_entries\":[]"), "{missing}");
    assert!(missing.contains("\"ws_log\":2"), "{missing}");
}

#[test]
fn dual_quaternion_skinning_changes_the_package_frame() {
    // N726: DQS and LBS are both available; opposing joint rotations make their
    // output visibly different, while the default mode stays LBS.
    use mdc_bake::{SceneClip, ShadeMode, SkinMode};
    let identity = mdc_bake::mat4_identity();
    let quarter = std::f32::consts::FRAC_1_SQRT_2;
    // The nodes below carry +90/-90 degrees: LBS averages the rotations to a
    // zero matrix, while DQS blends them back to identity (the triangle survives).
    let geometry = mdc_model::MeshGeometry {
        positions: vec![[1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]],
        indices: vec![0, 1, 2],
        skin_joints: vec![[1, 2, 0, 0]; 3],
        skin_weights: vec![[0.5, 0.5, 0.0, 0.0]; 3],
        ..Default::default()
    };
    let package = mdc_model::DevicePackage {
        target: "linux-mesh".into(),
        mode: mdc_model::PackageMode::Mesh,
        nodes: vec![
            mdc_model::Node {
                name: "root".into(),
                mesh: Some(0),
                ..Default::default()
            },
            mdc_model::Node {
                name: "plus".into(),
                rotation: [0.0, 0.0, quarter, quarter],
                ..Default::default()
            },
            mdc_model::Node {
                name: "minus".into(),
                rotation: [0.0, 0.0, -quarter, quarter],
                ..Default::default()
            },
        ],
        meshes: vec![mdc_model::Mesh {
            name: "tri".into(),
            vertices: 3,
            indices: 3,
            geometry: Some(geometry),
            ..Default::default()
        }],
        textures: Vec::new(),
        materials: Vec::new(),
        lights: Vec::new(),
        skeletons: vec![mdc_model::Skeleton {
            joint_nodes: vec![0, 1, 2],
            inverse_bind: vec![identity, identity, identity],
            ..Default::default()
        }],
        animations: Vec::new(),
        sprites: Vec::new(),
        layers: Vec::new(),
        estimated_bytes: 0,
        atlas: None,
        texture: None,
        texture_blobs: Vec::new(),
    };
    let render = |mode| {
        mdc_bake::render_package_scene_clip_skinned(
            &package,
            SceneClip::Static,
            0.0,
            (16, 16),
            mdc_bake::Filter::Nearest,
            ShadeMode::Flat,
            mode,
        )
        .unwrap()
        .rgb565
    };
    let lbs = render(SkinMode::Lbs);
    let dqs = render(SkinMode::Dqs);
    assert_ne!(lbs, dqs, "DQS and LBS must not collapse to the same frame");
    assert!(dqs.iter().any(|byte| *byte != 0), "DQS produced pixels");
    assert_eq!(SkinMode::default(), SkinMode::Lbs);

    // N746: the mesh-package renderer honours the same mode for skinned meshes.
    let mesh_lbs =
        mdc_bake::render_package_mesh_skinned(&package, (16, 16), ShadeMode::Flat, SkinMode::Lbs)
            .unwrap()
            .rgb565;
    let mesh_dqs =
        mdc_bake::render_package_mesh_skinned(&package, (16, 16), ShadeMode::Flat, SkinMode::Dqs)
            .unwrap()
            .rgb565;
    assert_ne!(mesh_lbs, mesh_dqs);
}

#[test]
fn per_material_alpha_cutoff_matches_on_the_device() {
    // N731: the material cutoff thresholds the texture at bake time; the host
    // package renderer and the device double see the same cutout.
    use image::RgbaImage;
    let image = RgbaImage::from_raw(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 100]).unwrap();
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let limits = mdc_bake::BakeLimits::default();
    let blob = mdc_bake::bake_texture_blob_with_cutoff(&png, &limits, 200).unwrap();
    let geometry = mdc_model::MeshGeometry {
        positions: vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
        uv: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        ..Default::default()
    };
    let model = mdc_model::Model {
        nodes: vec![mdc_model::Node {
            name: "root".into(),
            mesh: Some(0),
            ..Default::default()
        }],
        meshes: vec![mdc_model::Mesh {
            name: "quad".into(),
            vertices: 4,
            indices: 6,
            material: Some(0),
            geometry: Some(geometry),
            ..Default::default()
        }],
        materials: vec![mdc_model::Material {
            name: "cutout".into(),
            alpha_mode: Some("MASK".into()),
            alpha_cutoff: Some(0.8),
            ..Default::default()
        }],
        ..Default::default()
    };
    let package = mdc_bake::mesh_package_with_texture(
        &model,
        &mdc_model::RendererCapability::full_3d(),
        Some(blob),
    )
    .unwrap();
    let frame = mdc_bake::render_package_scene_clip_skinned(
        &package.package,
        mdc_bake::SceneClip::Static,
        0.0,
        (8, 8),
        mdc_bake::Filter::Nearest,
        mdc_bake::ShadeMode::Flat,
        mdc_bake::SkinMode::Lbs,
    )
    .unwrap();
    let (link, mut device) = FakeDevice::pair(BoardProfile {
        width: 8,
        height: 8,
        renderer_mesh2d: true,
        ..BoardProfile::default()
    });
    let mut session = Session::new(link);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    session
        .install_package(&package.to_bytes().unwrap())
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_preview());
    assert_eq!(device.pixels(), frame.rgb565.as_slice());
    // Left texel (alpha 255) survives; right texel (alpha 100 < 200) is cut.
    let pixel = |frame: &[u8], x: usize, y: usize| {
        let index = (y * 8 + x) * 2;
        u16::from_le_bytes([frame[index], frame[index + 1]])
    };
    assert_ne!(pixel(&frame.rgb565, 1, 4), 0, "left half is opaque");
    assert_eq!(pixel(&frame.rgb565, 6, 4), 0, "right half is cut out");
}

#[test]
fn scene_arc_mask_and_group_skew_reach_the_device() {
    // N753/N754/N759: arc/smooth paths, mask invert+feather and group shear all
    // rasterize on the device double (not just in the protocol parser).
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 24,
        height: 16,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    let show = |session: &mut Session<_>, device: &mut FakeDevice, scene: &str| {
        session
            .set_scene(serde_json::from_str(scene).unwrap())
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_scene());
    };
    show(
        &mut session,
        &mut device,
        r##"{"name":"root","kind":"path",
             "value2":"M2,12;A6,6,0,0,1,14,12;T22,12","value":"#FFFFFF"}"##,
    );
    assert_ne!(device.pixel_rgb565(8, 6), Some(0), "arc top");
    assert_ne!(device.pixel_rgb565(22, 12), Some(0), "smooth quad end");

    // N763: lowercase commands and `S` reach the device too.
    show(
        &mut session,
        &mut device,
        r##"{"name":"rel","kind":"path","value2":"M2,14;l20,0","value":"#FFFFFF"}"##,
    );
    assert_ne!(device.pixel_rgb565(12, 14), Some(0), "relative line");

    show(
        &mut session,
        &mut device,
        r##"{"name":"m","kind":"mask","value2":"0,0;8,0;8,8;0,8","mode":"invert",
             "children":[{"name":"fill","kind":"rect","rect":[0,0,16,8],"value":"#FF0000"}]}"##,
    );
    assert_eq!(
        device.pixel_rgb565(2, 4),
        Some(0),
        "inverted mask hides left"
    );
    assert_eq!(device.pixel_rgb565(12, 4), Some(0xF800), "right shown");

    show(
        &mut session,
        &mut device,
        r##"{"name":"g","kind":"group","value2":"500,0","children":[
             {"name":"bar","kind":"rect","rect":[0,0,4,12],"value":"#FFFFFF"}]}"##,
    );
    assert_ne!(device.pixel_rgb565(6, 10), Some(0), "sheared right");
    assert_eq!(device.hit_test(6, 10).as_deref(), Some("bar"));
}

#[test]
fn material_atlas_trims_transparent_margins() {
    // N760: both textures have a 2×2 opaque island in an 8×8 transparent field.
    // Packing trims each first, so the page is 16×2 rather than 16×8, and the
    // remapped UV still reaches the original colour.
    let mask = |x0: usize, y0: usize| {
        let mut bytes = vec![0u8; 8];
        for y in y0..y0 + 2 {
            for x in x0..x0 + 2 {
                let index = y * 8 + x;
                bytes[index / 8] |= 1 << (index % 8);
            }
        }
        bytes
    };
    let blob = |color: u16, x0: usize, y0: usize| mdc_model::TextureBlob {
        width: 8,
        height: 8,
        rgb565: color.to_le_bytes().repeat(64),
        alpha: mask(x0, y0),
        palette: Vec::new(),
        indices: Vec::new(),
        // N769: a source chain that must be rebuilt for the trimmed page.
        mips: vec![
            mdc_model::TextureMip {
                width: 4,
                height: 4,
                rgb565: color.to_le_bytes().repeat(16),
            },
            mdc_model::TextureMip {
                width: 2,
                height: 2,
                rgb565: color.to_le_bytes().repeat(4),
            },
            mdc_model::TextureMip {
                width: 1,
                height: 1,
                rgb565: color.to_le_bytes().to_vec(),
            },
        ],
    };
    let mesh = |material: usize, uv: [f32; 2]| mdc_model::Mesh {
        name: format!("quad-{material}"),
        vertices: 1,
        indices: 0,
        material: Some(material),
        geometry: Some(mdc_model::MeshGeometry {
            positions: vec![[0.0, 0.0, 0.0]],
            indices: vec![0],
            uv: vec![uv],
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut package = mdc_model::DevicePackage {
        target: "esp32-plane".into(),
        mode: mdc_model::PackageMode::Mesh,
        nodes: vec![mdc_model::Node {
            name: "root".into(),
            mesh: Some(0),
            ..Default::default()
        }],
        meshes: vec![mesh(0, [0.625, 0.625]), mesh(1, [0.125, 0.125])],
        textures: Vec::new(),
        materials: vec![
            mdc_model::Material {
                name: "red".into(),
                texture: Some(0),
                ..Default::default()
            },
            mdc_model::Material {
                name: "green".into(),
                texture: Some(1),
                ..Default::default()
            },
        ],
        lights: Vec::new(),
        skeletons: Vec::new(),
        animations: Vec::new(),
        sprites: Vec::new(),
        layers: Vec::new(),
        estimated_bytes: 512,
        atlas: None,
        texture: None,
        texture_blobs: vec![blob(0xF800, 4, 4), blob(0x07E0, 0, 0)],
    };
    assert!(mdc_bake::pack_material_atlas(&mut package, 16));
    assert_eq!(package.texture_blobs.len(), 1);
    let page = &package.texture_blobs[0];
    assert_eq!((page.width, page.height), (16, 2), "trimmed page height");
    assert_eq!(page.alpha.len(), 4, "alpha mask follows the trimmed page");
    // N769: the packed page carries a rebuilt chain ending at 1×1.
    assert!(!page.mips.is_empty());
    let last = page.mips.last().unwrap();
    assert_eq!((last.width, last.height), (1, 1));
    assert!(page.validate(), "trimmed page and mips validate");
    let sample = |mesh: &mdc_model::Mesh| {
        let [u, v] = mesh.geometry.as_ref().unwrap().uv[0];
        let x = ((u * page.width as f32).floor() as usize).min(page.width as usize - 1);
        let y = ((v * page.height as f32).floor() as usize).min(page.height as usize - 1);
        let index = (y * page.width as usize + x) * 2;
        u16::from_le_bytes([page.rgb565[index], page.rgb565[index + 1]])
    };
    assert_eq!(sample(&package.meshes[0]), 0xF800, "red UV still red");
    assert_eq!(sample(&package.meshes[1]), 0x07E0, "green UV still green");
}
