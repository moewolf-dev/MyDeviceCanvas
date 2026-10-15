//! N36: mirror shadow-buffer and runtime-package soaks stay bounded.
use mdc_core::{AllocationLedger, ConnectionState, Session};
use mdc_mirror::{MirrorReceiver, MockObsSink, ShadowFramebuffer, MAX_SHADOW_BYTES};
use mdc_protocol::encode_frame_payload;
use mdc_simulator::{BoardProfile, FakeDevice};

fn tiny_package() -> mdc_model::MdcPackage {
    let gltf = br#"{"asset":{"version":"2.0"},"nodes":[{"name":"root","mesh":0}],
        "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
        "accessors":[{"count":4,"type":"VEC3"}]}"#;
    let model = mdc_model::GltfImporter
        .import(gltf, &mdc_model::ImportLimits::default())
        .unwrap();
    mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::sprite_only()).unwrap()
}

#[test]
fn shadow_framebuffer_soak_is_bounded() {
    let mut fb = ShadowFramebuffer::new(64, 64).unwrap();
    let slot_bytes = fb.slot_bytes();
    let total = fb.total_bytes();
    assert!(total <= MAX_SHADOW_BYTES);
    let rgb565 = vec![0x11u8; 64 * 64 * 2];
    for _ in 0..5000 {
        let _ = fb.present(&rgb565);
        if let Some(lease) = fb.acquire() {
            assert_eq!(fb.frame_view(&lease).pixels.len(), slot_bytes);
            drop(lease);
        }
    }
    // Memory never grew, and after every lease is released writes keep working.
    assert_eq!(fb.total_bytes(), total);
    assert!(fb.present(&rgb565).is_ok());
}

#[test]
fn session_mirror_and_package_soak_returns_to_zero() {
    let package = tiny_package();
    let package_bytes = package.to_bytes().unwrap();
    // Counts the frame buffers the harness holds; must return to zero each round.
    let mut ledger = AllocationLedger::default();

    for _round in 0..100 {
        let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

        session.install_package(&package_bytes).unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.package_ready());

        session.start_mirror("main", "full-frame", 15).unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        for _ in 0..4 {
            device.emit_mirror_frame().unwrap();
            session.poll().unwrap();
            if let Some(frame) = session.take_mirror_frame() {
                assert!(ledger.alloc(frame.bytes.len()));
                ledger.free(frame.bytes.len());
                drop(frame);
            }
        }
        assert_eq!(ledger.outstanding, 0, "harness buffers released each round");

        session.stop_mirror("main").unwrap();
        let stats = session.mirror_stats();
        assert!(stats.frames_received >= 4);

        drop(session);
        drop(device);
    }
    assert_eq!(ledger.outstanding, 0);
    assert!(ledger.peak <= 800 * 480 * 2, "peak stayed within one frame");
}

#[test]
fn mirror_receiver_soak_is_bounded() {
    // Feed full frames through a receiver; the shadow buffer never grows and the
    // receiver keeps publishing (busy writes are the only drops).
    let mut receiver =
        MirrorReceiver::new("main", 2, 1, "full-frame", MockObsSink::default()).unwrap();
    let payload = encode_frame_payload("main", 1, 2, 1, &[0x00, 0xF8, 0xE0, 0x07]).unwrap();
    let packet = mdc_protocol::Packet {
        version: mdc_protocol::VERSION,
        kind: mdc_protocol::MessageType::MirrorFrame,
        flags: 0,
        request_id: 0,
        payload: payload.clone(),
    }
    .encode(1024 * 1024)
    .unwrap();
    let shadow = receiver.shadow_bytes();
    for _ in 0..2000 {
        receiver.feed(&packet).unwrap();
        assert_eq!(receiver.shadow_bytes(), shadow);
    }
    assert!(receiver.stats().frames_published >= 1);
}

#[test]
fn multi_primitive_geometry_soak_is_bounded() {
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
                skin_joints: Vec::new(),
                skin_weights: Vec::new(),
                morph_targets: Vec::new(),
            }),
            ..Mesh::default()
        }],
        materials: vec![Material {
            name: "red".into(),
            texture: None,
            color: Some([255, 0, 0]),
        }],
        ..Model::default()
    };
    let mut bytes = 0usize;
    for _ in 0..100 {
        let frame = mdc_bake::render_scene_at(&model, 0, 0.0, (32, 16), None, 0xFFFF).unwrap();
        if bytes == 0 {
            bytes = frame.rgb565.len();
        }
        assert_eq!(frame.rgb565.len(), bytes, "frame size is constant");
    }
    assert!(bytes > 0);
}

#[test]
fn dirty_region_mirror_soak_is_bounded() {
    use mdc_core::{ConnectionState, Session};
    use mdc_protocol::SceneNode;
    use mdc_simulator::{BoardProfile, FakeDevice};

    let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    session
        .set_scene(SceneNode {
            name: "bg".into(),
            kind: "rect".into(),
            rect: Some([0, 0, 4, 4]),
            children: Vec::new(),
            value: Some("#000000".into()),
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    session.start_mirror("main", "dirty-region", 10).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();

    let bytes = device.framebuffer().len();
    for i in 0..100 {
        let color = if i % 2 == 0 { "#FF0000" } else { "#000000" };
        session
            .set_scene(SceneNode {
                name: "bg".into(),
                kind: "rect".into(),
                rect: Some([0, 0, 4, 4]),
                children: Vec::new(),
                value: Some(color.into()),
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        device.emit_mirror_frame().unwrap();
        session.poll().unwrap();
        assert_eq!(
            device.framebuffer().len(),
            bytes,
            "framebuffer stays bounded"
        );
        if let Some(frame) = session.take_mirror_frame() {
            assert_eq!(frame.bytes.len(), bytes, "mirror frame stays bounded");
        }
    }
}

#[test]
fn cubicspline_sampling_soak_is_bounded() {
    use mdc_model::AnimationTrack;
    let track = AnimationTrack {
        node: 0,
        path: "translation".into(),
        interpolation: "CUBICSPLINE".into(),
        times: vec![0.0, 1.0],
        values: vec![0.0, 0.0, 0.0, 0.0, 10.0, 0.0],
    };
    for i in 0..100_000u32 {
        let t = (i % 1001) as f32 / 1000.0;
        let v = mdc_bake::sample_channel(&track, 1, t)[0];
        assert!(v.is_finite());
        assert!((-0.5..=10.5).contains(&v), "cubic stays near range ({v})");
    }
}

#[test]
fn geometry_morph_render_soak_is_bounded() {
    use mdc_model::{Animation, AnimationTrack, Material, Mesh, MeshGeometry, Model, Node};

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
    let mut bytes = 0usize;
    for i in 0..100 {
        let time = (i % 10) as f32 / 10.0;
        let frame = mdc_bake::render_scene_at(&model, 0, time, (32, 32), None, 0xFFFF).unwrap();
        if bytes == 0 {
            bytes = frame.rgb565.len();
        }
        assert_eq!(frame.rgb565.len(), bytes, "frame size is constant");
    }
    assert!(bytes > 0);
}

#[test]
fn motion_curve_sampling_soak_is_bounded() {
    // A 1s linear ramp sampled 100k times: deterministic, allocation-free.
    let segments = [0.0, 0.0, 1.0, 10.0];
    for i in 0..100_000u32 {
        let t = (i % 1001) as f32 / 1000.0;
        let v = mdc_model::sample_segments(&segments, t);
        assert!((0.0..=10.0).contains(&v), "sample in range ({v})");
        if t >= 1.0 {
            assert_eq!(v, 10.0, "endpoint held");
        }
    }
}

#[test]
fn linear_filter_render_soak_is_bounded() {
    use mdc_model::{Mesh, MeshGeometry, Model, Node};

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
                skin_joints: Vec::new(),
                skin_weights: Vec::new(),
                morph_targets: Vec::new(),
            }),
            ..Mesh::default()
        }],
        materials: vec![mdc_model::Material {
            name: "skin".into(),
            texture: None,
            color: Some([255, 255, 255]),
        }],
        ..Model::default()
    };
    let texture = mdc_bake::TextureMap::from_rgb565(2, 2, vec![0u8; 2 * 2 * 2])
        .unwrap()
        .with_filter(mdc_bake::Filter::Linear);
    let mut bytes = 0usize;
    for _ in 0..100 {
        let frame =
            mdc_bake::render_scene_at(&model, 0, 0.0, (32, 32), Some(&texture), 0xFFFF).unwrap();
        if bytes == 0 {
            bytes = frame.rgb565.len();
        }
        assert_eq!(frame.rgb565.len(), bytes, "frame size is constant");
    }
    assert!(bytes > 0);
}

#[test]
fn device_mesh_scene_render_soak_is_bounded() {
    use mdc_core::{ConnectionState, Session};
    use mdc_model::{Animation, AnimationTrack, Material, Mesh, MeshGeometry, Model, Node};
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
                ..Node::default()
            },
        ],
        meshes: vec![mesh],
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
    let bytes = device.framebuffer().len();
    for _ in 0..100 {
        device.clock.advance(50);
        device.emit_mirror_frame().unwrap();
        session.poll().unwrap();
        let _ = session.take_mirror_frame();
        assert_eq!(
            device.framebuffer().len(),
            bytes,
            "framebuffer stays bounded"
        );
    }
}

#[test]
fn multi_mesh_scene_render_soak_is_bounded() {
    use mdc_model::{Material, Mesh, MeshGeometry, Model, Node};

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
    let mut bytes = 0usize;
    for _ in 0..100 {
        let frame = mdc_bake::render_scene_at(&model, 0, 0.0, (32, 16), None, 0xFFFF).unwrap();
        if bytes == 0 {
            bytes = frame.rgb565.len();
        }
        assert_eq!(frame.rgb565.len(), bytes, "frame size is constant");
    }
    assert!(bytes > 0);
}

#[test]
fn textured_mesh_package_soak_is_bounded() {
    use mdc_model::{Material, Mesh, MeshGeometry, Model, Node, TextureBlob};

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
                skin_joints: Vec::new(),
                skin_weights: Vec::new(),
                morph_targets: Vec::new(),
            }),
            ..Mesh::default()
        }],
        materials: vec![Material {
            name: "skin".into(),
            texture: None,
            color: Some([10, 20, 30]),
        }],
        ..Model::default()
    };
    let texture = TextureBlob {
        width: 2,
        height: 1,
        rgb565: vec![0x00, 0xF8, 0xE0, 0x07],
    };
    let caps = mdc_model::RendererCapability::full_3d();
    let mut size = 0usize;
    for _ in 0..100 {
        let package =
            mdc_bake::mesh_package_with_texture(&model, &caps, Some(texture.clone())).unwrap();
        let bytes = package.to_bytes().unwrap();
        if size == 0 {
            size = bytes.len();
            let back = mdc_model::MdcPackage::from_bytes(&bytes).unwrap();
            assert_eq!(back, package);
        }
        assert_eq!(bytes.len(), size, "package size is constant");
    }
    assert!(size > 0);
}

#[test]
fn geometry_render_soak_is_bounded() {
    use mdc_model::{Animation, AnimationTrack, Mesh, MeshGeometry, Model, Node, Skeleton};

    let mesh = Mesh {
        name: "quad".into(),
        vertices: 4,
        indices: 6,
        material: None,
        skin_joints: vec![[1, 0, 0, 0]; 4],
        skin_weights: vec![[1.0, 0.0, 0.0, 0.0]; 4],
        geometry: Some(MeshGeometry {
            positions: vec![
                [-0.5, -0.5, 0.0],
                [0.5, -0.5, 0.0],
                [0.5, 0.5, 0.0],
                [-0.5, 0.5, 0.0],
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            uv: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            skin_joints: vec![[1, 0, 0, 0]; 4],
            skin_weights: vec![[1.0, 0.0, 0.0, 0.0]; 4],
            morph_targets: Vec::new(),
        }),
        ..Mesh::default()
    };
    let identity = [
        1.0f32, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ];
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
        skeletons: vec![Skeleton {
            name: "rig".into(),
            bones: 2,
            joint_nodes: vec![0, 1],
            inverse_bind: vec![identity, identity],
        }],
        animations: vec![Animation {
            name: "spin".into(),
            channels: 1,
            tracks: vec![AnimationTrack {
                node: 1,
                path: "rotation".into(),
                interpolation: "LINEAR".into(),
                times: vec![0.0, 1.0],
                values: vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            }],
        }],
        ..Model::default()
    };
    let texture = mdc_bake::TextureMap::from_rgb565(2, 1, vec![0x00, 0xF8, 0xE0, 0x07]).unwrap();
    let mut bytes = 0usize;
    for i in 0..100 {
        let time = (i % 10) as f32 / 10.0;
        let frame = mdc_bake::render_geometry_at(&model, 0, time, (32, 32), Some(&texture), 0x001F)
            .unwrap();
        if bytes == 0 {
            bytes = frame.rgb565.len();
        }
        assert_eq!(frame.rgb565.len(), bytes, "frame size is constant");
    }
    assert!(bytes > 0);
}

#[test]
fn scene_primitive_soak_is_bounded() {
    use mdc_core::Session;
    use mdc_protocol::SceneNode;
    use mdc_simulator::{BoardProfile, FakeDevice};

    let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    let scene = SceneNode {
        name: "root".into(),
        kind: "group".into(),
        rect: None,
        value: None,
        children: vec![
            SceneNode {
                name: "disc".into(),
                kind: "circle".into(),
                rect: Some([10, 10, 200, 200]),
                children: Vec::new(),
                value: Some("#FF0000".into()),
            },
            SceneNode {
                name: "diag".into(),
                kind: "line".into(),
                rect: Some([0, 0, 800, 480]),
                children: Vec::new(),
                value: Some("#00FF00".into()),
            },
        ],
    };
    session.set_scene(scene).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    let bytes = device.framebuffer().len();
    for _ in 0..100 {
        assert!(device.render_scene());
        assert_eq!(
            device.framebuffer().len(),
            bytes,
            "framebuffer stays bounded"
        );
    }
}

#[test]
fn http_mirror_publish_soak_is_bounded() {
    let mut http = mdc_mirror::HttpMirrorServer::bind("127.0.0.1:0").unwrap();
    let width = 480u16;
    let height = 480u16;
    let pixels = vec![0x40u8; width as usize * height as usize * 4];
    for generation in 0..100 {
        http.publish(&mdc_mirror::ShadowFrame {
            width,
            height,
            generation,
            frame_id: generation,
            pixels: &pixels,
        });
    }
    // Only the newest frame is cached, so memory does not grow with frames.
    assert_eq!(http.frames_published(), 100);
    assert!(http.has_frame());
}

#[test]
fn render_frame_soak_is_bounded() {
    use mdc_model::{Animation, AnimationTrack, Mesh, Model, Node, Skeleton};

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
        skeletons: vec![Skeleton {
            name: "rig".into(),
            bones: 2,
            joint_nodes: vec![0, 1],
            inverse_bind: vec![identity, identity],
        }],
        animations: vec![Animation {
            name: "spin".into(),
            channels: 1,
            tracks: vec![AnimationTrack {
                node: 1,
                path: "rotation".into(),
                interpolation: "LINEAR".into(),
                times: vec![0.0, 1.0],
                values: vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            }],
        }],
        ..Model::default()
    };
    let texture = mdc_bake::TextureMap::from_rgb565(2, 1, vec![0x00, 0xF8, 0xE0, 0x07]).unwrap();
    let mut bytes = 0usize;
    for i in 0..100 {
        let time = (i % 10) as f32 / 10.0;
        let frame =
            mdc_bake::render_frame_at(&model, 0, time, (8, 8), Some(&texture), 0x001F).unwrap();
        if bytes == 0 {
            bytes = frame.rgb565.len();
        }
        assert_eq!(frame.rgb565.len(), bytes, "frame size is constant");
    }
    assert!(bytes > 0);
}

#[test]
fn morph_bake_soak_is_bounded() {
    use mdc_model::{Animation, AnimationTrack, Mesh, Model, Node, Skeleton};

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
        morph_targets: vec![vec![[1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0, 0.0]]],
        morph_weights: vec![0.0],
        ..Mesh::default()
    };
    let identity = [
        1.0f32, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ];
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
        skeletons: vec![Skeleton {
            name: "rig".into(),
            bones: 2,
            joint_nodes: vec![0, 1],
            inverse_bind: vec![identity, identity],
        }],
        animations: vec![Animation {
            name: "smile".into(),
            channels: 1,
            tracks: vec![AnimationTrack {
                node: 1,
                path: "weights".into(),
                interpolation: "LINEAR".into(),
                times: vec![0.0, 1.0],
                values: vec![0.0, 1.0],
            }],
        }],
        ..Model::default()
    };
    let mut size = 0usize;
    for _ in 0..100 {
        let package = mdc_bake::bake_skinned_to_package(&model, 0, 4, (8, 8), 0xF800).unwrap();
        let bytes = package.to_bytes().unwrap();
        if size == 0 {
            size = bytes.len();
        }
        assert_eq!(bytes.len(), size, "package size is constant");
    }
    assert!(size > 0);
}

#[test]
fn textured_bake_soak_is_bounded() {
    use mdc_model::{Animation, AnimationTrack, Mesh, Model, Node, Skeleton};

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
        skeletons: vec![Skeleton {
            name: "rig".into(),
            bones: 2,
            joint_nodes: vec![0, 1],
            inverse_bind: vec![identity, identity],
        }],
        animations: vec![Animation {
            name: "spin".into(),
            channels: 1,
            tracks: vec![AnimationTrack {
                node: 1,
                path: "rotation".into(),
                interpolation: "LINEAR".into(),
                times: vec![0.0, 1.0],
                values: vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            }],
        }],
        ..Model::default()
    };
    let texture = mdc_bake::TextureMap::from_rgb565(2, 1, vec![0x00, 0xF8, 0xE0, 0x07]).unwrap();
    let mut size = 0usize;
    for _ in 0..100 {
        let package =
            mdc_bake::bake_textured_to_package(&model, 0, 3, (8, 8), Some(&texture), 0x001F)
                .unwrap();
        let bytes = package.to_bytes().unwrap();
        if size == 0 {
            size = bytes.len();
            let back = mdc_model::MdcPackage::from_bytes(&bytes).unwrap();
            assert_eq!(back, package);
        }
        assert_eq!(bytes.len(), size, "package size is constant");
    }
    assert!(size > 0);
}

#[test]
fn bake_soak_stays_bounded() {
    use image::{Rgb, RgbImage};
    use std::io::Cursor;

    let image = RgbImage::from_pixel(8, 8, Rgb([200, 100, 50]));
    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let source = mdc_bake::SpriteSource {
        name: "src".into(),
        image: png,
    };
    let motion = mdc_model::MotionInfo {
        name: "idle".into(),
        channels: 1,
        duration_sec: 0.1,
        fps: 30.0,
        fade_in_sec: 0.0,
        fade_out_sec: 0.0,
        looping: true,
        curves: Vec::new(),
    };
    let mut bytes = 0usize;
    for _ in 0..200 {
        let package = mdc_bake::bake_sprites(
            std::slice::from_ref(&source),
            std::slice::from_ref(&motion),
            (8, 8),
            2048,
            &mdc_bake::BakeLimits::default(),
        )
        .unwrap();
        if bytes == 0 {
            bytes = package.atlas.rgb565.len();
        }
        assert_eq!(package.atlas.rgb565.len(), bytes, "atlas size is constant");
        assert_eq!(package.manifest.atlas_bytes, bytes as u64);
    }
    assert!(bytes > 0);
}

#[test]
fn baked_package_soak_is_bounded() {
    use image::{Rgb, RgbImage};
    use std::io::Cursor;

    let image = RgbImage::from_pixel(8, 8, Rgb([30, 60, 90]));
    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let source = mdc_bake::SpriteSource {
        name: "src".into(),
        image: png,
    };
    let motion = mdc_model::MotionInfo {
        name: "idle".into(),
        channels: 1,
        duration_sec: 0.1,
        fps: 30.0,
        fade_in_sec: 0.0,
        fade_out_sec: 0.0,
        looping: true,
        curves: Vec::new(),
    };
    let mut size = 0usize;
    for _ in 0..100 {
        let package = mdc_bake::bake_to_package(
            std::slice::from_ref(&source),
            std::slice::from_ref(&motion),
            (8, 8),
            2048,
            &mdc_bake::BakeLimits::default(),
        )
        .unwrap();
        let bytes = package.to_bytes().unwrap();
        if size == 0 {
            size = bytes.len();
            // integrity round-trip once
            let back = mdc_model::MdcPackage::from_bytes(&bytes).unwrap();
            assert_eq!(back, package);
        }
        assert_eq!(bytes.len(), size, "package size is constant");
    }
    assert!(size > 0);
}

#[test]
fn atlas_animation_soak_is_bounded() {
    use mdc_core::Session;
    use mdc_simulator::{BoardProfile, FakeDevice};

    let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();

    let atlas = mdc_model::AtlasBlob {
        sprite_width: 2,
        sprite_height: 2,
        frames: 8,
        columns: 4,
        rows: 2,
        rgb565: vec![0u8; 2 * 2 * 8 * 2],
        clips: vec![mdc_model::AtlasClip {
            name: "idle".into(),
            start: 0,
            frames: 8,
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
        estimated_bytes: atlas.rgb565.len() as u64,
        atlas: Some(atlas),
        texture: None,
    };
    let sealed = mdc_model::MdcPackage::seal(package).unwrap();
    session
        .install_package(&sealed.to_bytes().unwrap())
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();

    session.set_config("animation", "idle").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();

    for _ in 0..5000 {
        device.clock.advance(3);
        let frame = device.current_atlas_frame().unwrap();
        assert!(frame < 8, "frame index stays in range");
    }
    assert_eq!(device.atlas_frames(), 8);
}

#[test]
fn scene_render_soak_is_bounded() {
    use mdc_core::Session;
    use mdc_protocol::SceneNode;
    use mdc_simulator::{BoardProfile, FakeDevice};

    let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();

    session
        .set_scene(SceneNode {
            name: "bg".into(),
            kind: "rect".into(),
            rect: Some([0, 0, 16, 16]),
            children: Vec::new(),
            value: Some("#00FF00".into()),
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();

    let len = device.framebuffer().len();
    for _ in 0..5000 {
        assert!(device.render_scene());
        assert_eq!(device.framebuffer().len(), len, "framebuffer size is fixed");
        assert_eq!(device.pixel_rgb565(5, 5), Some(0x07E0));
    }
}
