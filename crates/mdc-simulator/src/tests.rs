use super::*;
use mdc_core::{ConnectionState, Frame, Session, Tile};
use mdc_protocol::OtaCommand;
use mdc_transport::MemoryLink;

#[test]
fn config_set_get_validate_and_redact() {
    let (host, mut device) = FakeDevice::pair(BoardProfile::default());
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

    session.set_config("brightness", "200").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(session.config("brightness"), Some("200"));
    assert_eq!(device.config_value("brightness"), Some("200"));

    // An invalid value is rejected by the device and keeps the old value.
    session.set_config("brightness", "999").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(session.config("brightness"), Some("200"));

    // A secret is never echoed back.
    session.get_config("wifi_password").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(session.config("wifi_password"), Some("***"));
    assert_eq!(device.config_value("wifi_password"), Some("mdc-sim-secret"));
    assert!(device.metrics.config_commands >= 3);
}

#[test]
fn scene_set_and_clear_are_capability_gated() {
    use mdc_protocol::SceneNode;
    let (host, mut device) = FakeDevice::pair(BoardProfile::default());
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

    let leaf = |name: &str, kind: &str, value: Option<&str>| SceneNode {
        name: name.into(),
        kind: kind.into(),
        rect: None,
        children: Vec::new(),
        value: value.map(str::to_string),
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
    let mut root = leaf("root", "rect", None);
    root.children.push(leaf("label", "text", Some("hi")));
    session.set_scene(root).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.has_scene());
    assert_eq!(device.scene_node_count(), 2);

    session.clear_scene().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(!device.has_scene());
    assert!(device.metrics.scene_commands >= 2);
}

#[test]
fn accepts_a_baked_sprites_package_and_records_the_grid() {
    let (host, mut device) = FakeDevice::pair(BoardProfile::default());
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

    let atlas = mdc_model::AtlasBlob {
        sprite_width: 2,
        sprite_height: 2,
        frames: 2,
        columns: 2,
        rows: 1,
        rgb565: vec![0u8; 2 * 2 * 2 * 2],
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
    assert!(device.package_ready());
    assert_eq!(device.atlas_frames(), 2);
    assert_eq!(device.atlas_sprite(), (2, 2));
}

#[test]
fn atlas_animation_runtime_indexes_frames_by_the_clock() {
    let (host, mut device) = FakeDevice::pair(BoardProfile::default());
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

    // Install a baked atlas with a 4-frame looping clip.
    let atlas = mdc_model::AtlasBlob {
        sprite_width: 2,
        sprite_height: 2,
        frames: 4,
        columns: 2,
        rows: 2,
        rgb565: vec![0u8; 2 * 2 * 4 * 2],
        clips: vec![mdc_model::AtlasClip {
            name: "idle".into(),
            start: 0,
            frames: 4,
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
        estimated_bytes: atlas.rgb565.len() as u64,
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
    assert!(device.package_ready());
    assert_eq!(device.current_atlas_frame(), None, "no clip running yet");

    session.set_config("animation", "idle").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.active_clip(), Some("idle"));
    assert_eq!(device.current_atlas_frame(), Some(0));
    assert_eq!(device.animation_loops(), 0);

    // 40ms at 30fps -> frame 1; 4 frames loop.
    device.clock.advance(40);
    assert_eq!(device.current_atlas_frame(), Some(1));
    assert_eq!(device.animation_loops(), 0);
    device.clock.advance(4 * 1000);
    assert_eq!(device.current_atlas_frame(), Some(1));
    assert!(
        device.animation_loops() >= 1,
        "the clip wrapped at least once"
    );

    session.set_config("animation", "off").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.current_atlas_frame(), None);
}

#[test]
fn animation_crossfades_between_clips() {
    let (host, mut device) = FakeDevice::pair(BoardProfile::default());
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

    // A 2-frame atlas: frame 0 red, frame 1 green. "wave" cross-fades in 1s.
    let atlas = mdc_model::AtlasBlob {
        sprite_width: 1,
        sprite_height: 1,
        frames: 2,
        columns: 2,
        rows: 1,
        rgb565: vec![0x00, 0xF8, 0xE0, 0x07],
        clips: vec![
            mdc_model::AtlasClip {
                name: "idle".into(),
                start: 0,
                frames: 1,
                fps: 1,
                looping: true,
                fade_in_ms: 0,
                fade_out_ms: 0,
                mode: None,
                speed_permille: None,
            },
            mdc_model::AtlasClip {
                name: "wave".into(),
                start: 1,
                frames: 1,
                fps: 1,
                looping: true,
                fade_in_ms: 1000,
                fade_out_ms: 0,
                mode: None,
                speed_permille: None,
            },
        ],

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
        estimated_bytes: 8,
        atlas: Some(atlas),
        texture: None,
        texture_blobs: Vec::new(),
    };
    session
        .install_package(
            &mdc_model::MdcPackage::seal(package)
                .unwrap()
                .to_bytes()
                .unwrap(),
        )
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();

    session.set_config("animation", "idle").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_atlas_frame());
    assert_eq!(device.pixel_rgb565(0, 0), Some(0xF800), "idle is red");

    // Switching to wave: at t=0 the frozen idle frame still wins.
    session.set_config("animation", "wave").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_atlas_frame());
    assert_eq!(
        device.pixel_rgb565(0, 0),
        Some(0xF800),
        "old frame covers at t=0"
    );

    // After the 1s window the cross-fade has finished: pure green.
    device.clock.advance(1000);
    assert!(device.render_atlas_frame());
    assert_eq!(
        device.pixel_rgb565(0, 0),
        Some(0x07E0),
        "new frame after blend"
    );
}

#[test]
fn animation_blend_averages_atlas_clips() {
    // N834/N835: two weighted one-frame clips mix their RGB565 pixels, an
    // unknown name is refused, and a single clip clears the blend.
    let (host, mut device) = FakeDevice::pair(BoardProfile::default());
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

    let atlas = mdc_model::AtlasBlob {
        sprite_width: 1,
        sprite_height: 1,
        frames: 2,
        columns: 2,
        rows: 1,
        rgb565: vec![0x00, 0xF8, 0xE0, 0x07],
        clips: vec![
            mdc_model::AtlasClip {
                name: "idle".into(),
                start: 0,
                frames: 1,
                fps: 1,
                looping: true,
                fade_in_ms: 0,
                fade_out_ms: 0,
                mode: None,
                speed_permille: None,
            },
            mdc_model::AtlasClip {
                name: "wave".into(),
                start: 1,
                frames: 1,
                fps: 1,
                looping: true,
                fade_in_ms: 0,
                fade_out_ms: 0,
                mode: None,
                speed_permille: None,
            },
        ],
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
        estimated_bytes: 8,
        atlas: Some(atlas),
        texture: None,
        texture_blobs: Vec::new(),
    };
    session
        .install_package(
            &mdc_model::MdcPackage::seal(package)
                .unwrap()
                .to_bytes()
                .unwrap(),
        )
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();

    session.set_animation_blend("idle:500,wave:500").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.animation_blend().len(), 2);
    assert_eq!(session.active_animation_blend(), Some("idle:500,wave:500"));
    assert_eq!(
        session.active_animation(),
        None,
        "the blend replaced the cached single clip"
    );
    assert!(device.render_atlas_frame());
    assert_eq!(
        device.pixel_rgb565(0, 0),
        Some(0x8400),
        "red + green at equal weights"
    );

    // An unknown clip keeps the previous blend.
    session.set_config("animation_blend", "nope:500").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.animation_blend().len(), 2, "old blend survives");

    // A single clip replaces the blend.
    session.set_animation("idle").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.animation_blend().is_empty());
    assert_eq!(device.active_clip(), Some("idle"));
    assert_eq!(session.active_animation_blend(), None);

    // `off` clears both.
    session.set_animation_blend("idle:500,wave:500").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    session.stop_animation_blend().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.animation_blend().is_empty());
    assert_eq!(session.active_animation_blend(), None);
}

#[test]
fn animation_blend_averages_mesh_package_clips() {
    // N836: two package clips pose the same mesh in different places; the
    // weighted frame contains pixels from both, not just one.
    use mdc_model::{Animation, AnimationTrack, Mesh, MeshGeometry, Node};
    let geometry = MeshGeometry {
        positions: vec![[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [0.0, 1.0, 0.0]],
        indices: vec![0, 1, 2],
        ..MeshGeometry::default()
    };
    let mesh = Mesh {
        name: "tri".into(),
        vertices: 3,
        indices: 3,
        geometry: Some(geometry),
        ..Mesh::default()
    };
    let clip = |name: &str, x: f32| Animation {
        name: name.into(),
        channels: 1,
        tracks: vec![AnimationTrack {
            node: 0,
            path: "translation".into(),
            interpolation: "LINEAR".into(),
            times: vec![0.0, 1.0],
            values: vec![x, 0.0, 0.0, x, 0.0, 0.0],
        }],
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
        animations: vec![clip("left", -0.6), clip("right", 0.6)],
        sprites: Vec::new(),
        layers: Vec::new(),
        estimated_bytes: 64,
        atlas: None,
        texture: None,
        texture_blobs: Vec::new(),
    };
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
        .install_package(
            &mdc_model::MdcPackage::seal(package)
                .unwrap()
                .to_bytes()
                .unwrap(),
        )
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.package_ready());

    let lit = |device: &FakeDevice| {
        (0..16)
            .flat_map(|y| (0..16).map(move |x| (x, y)))
            .filter(|(x, y)| device.pixel_rgb565(*x, *y).unwrap_or(0) != 0)
            .count()
    };
    session.set_animation("left").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    // Let the clip-change cross-fade finish before counting the frame.
    device.clock.advance(u64::from(MESH_CROSSFADE_MS) + 1);
    assert!(device.render_preview());
    let left = lit(&device);
    assert!(left > 0, "the single clip draws");
    assert!(device.mesh_animation_blend().is_empty());

    session.set_animation_blend("left:500,right:500").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.mesh_animation_blend().len(), 2);
    assert!(device.render_preview());
    let blended = lit(&device);
    assert!(
        blended > left,
        "the blended frame covers both poses ({blended} vs {left})"
    );
}

#[test]
fn device_renders_the_running_atlas_frame_into_the_mirror() {
    let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

    // A 4x2 atlas: row = [frame0, frame0, frame1, frame1] (red, green).
    let mut pixels = Vec::new();
    for _row in 0..2 {
        for value in [0xF800u16, 0xF800u16, 0x07E0u16, 0x07E0u16] {
            pixels.push((value & 0xff) as u8);
            pixels.push((value >> 8) as u8);
        }
    }
    let atlas = mdc_model::AtlasBlob {
        sprite_width: 2,
        sprite_height: 2,
        frames: 2,
        columns: 2,
        rows: 1,
        rgb565: pixels,
        clips: vec![mdc_model::AtlasClip {
            name: "idle".into(),
            start: 0,
            frames: 2,
            fps: 30,
            looping: false,
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
        estimated_bytes: atlas.rgb565.len() as u64,
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

    session.start_mirror("main", "full-frame", 15).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    session.set_config("animation", "idle").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();

    // 40ms at 30fps -> frame 1 (green).
    device.clock.advance(40);
    assert_eq!(device.current_atlas_frame(), Some(1));
    device.emit_mirror_frame().unwrap();
    session.poll().unwrap();
    let frame = session.take_mirror_frame().unwrap();
    assert_eq!(&frame.bytes[0..2], &[0xE0, 0x07], "second frame is green");
}

#[test]
fn scene_renders_rects_with_bounds_and_z_order() {
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

    let rect = |name: &str, geom: [u16; 4], color: &str| SceneNode {
        name: name.into(),
        kind: "rect".into(),
        rect: Some(geom),
        children: Vec::new(),
        value: Some(color.into()),
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
    // Background red covers all; a green child on top; a third clipped.
    let scene = SceneNode {
        name: "root".into(),
        kind: "rect".into(),
        rect: Some([0, 0, 4, 4]),
        children: vec![
            rect("green", [1, 1, 1, 1], "#00FF00"),
            rect("clip", [3, 3, 10, 10], "#0000FF"),
        ],
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
    };
    session.set_scene(scene).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_scene());
    assert_eq!(device.pixel_rgb565(0, 0), Some(0xF800), "red background");
    assert_eq!(device.pixel_rgb565(1, 1), Some(0x07E0), "green on top");
    assert_eq!(
        device.pixel_rgb565(3, 3),
        Some(0x001F),
        "clipped blue corner"
    );
}

#[test]
fn scene_draws_text_and_image_pixels() {
    use mdc_protocol::SceneNode;
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 16,
        height: 8,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    let scene = SceneNode {
        name: "root".into(),
        kind: "group".into(),
        rect: None,
        value: None,
        children: vec![
            SceneNode {
                name: "label".into(),
                kind: "text".into(),
                rect: Some([0, 0, 0, 0]),
                value: Some("H".into()),
                children: Vec::new(),
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
            },
            SceneNode {
                name: "swatch".into(),
                kind: "image".into(),
                rect: Some([12, 0, 1, 1]),
                value: Some("07e0".into()),
                children: Vec::new(),
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
            },
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
    assert_ne!(device.pixel_rgb565(0, 0), Some(0));
    assert_eq!(device.pixel_rgb565(12, 0), Some(0x07e0));
}

#[test]
fn image_node_scales_with_nearest_and_bilinear() {
    // N556: a 2x1 source (red, blue) scaled to a 4x1 rect.
    use mdc_protocol::SceneNode;
    let render = |mode: Option<&str>| {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 1,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "pic".into(),
            kind: "image".into(),
            rect: Some([0, 0, 4, 1]),
            value: Some("f800001f".into()),
            value2: Some("2,1".into()),
            children: Vec::new(),
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
        });
        assert!(device.render_scene());
        (0..4)
            .map(|x| device.pixel_rgb565(x, 0).unwrap())
            .collect::<Vec<_>>()
    };
    // Nearest: two columns of red then two of blue.
    let nearest = render(None);
    assert_eq!(nearest, vec![0xF800, 0xF800, 0x001F, 0x001F]);
    // Bilinear: the interior column is a blend, not an exact endpoint colour.
    let linear = render(Some("linear"));
    assert_ne!(linear[1], 0xF800, "bilinear blends");
    assert_ne!(linear[1], 0x001F, "bilinear blends");
    assert_eq!(linear[0], 0xF800, "the first column is still red");
    assert_eq!(linear[3], 0x001F, "the last column is still blue");
}

#[test]
fn image_node_supports_flip_and_rotate() {
    // N565: `mode` flip tokens and `rotate` transform the sampled image.
    use mdc_protocol::SceneNode;
    let render = |mode: Option<&str>, rotate: Option<u16>| {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 1,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "pic".into(),
            kind: "image".into(),
            rect: Some([0, 0, 4, 1]),
            value: Some("f800001f".into()),
            value2: Some("2,1".into()),
            children: Vec::new(),
            opacity: None,
            visible: None,
            anim: None,
            rotate,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: mode.map(str::to_string),
            focus: None,
        });
        assert!(device.render_scene());
        (0..4)
            .map(|x| device.pixel_rgb565(x, 0).unwrap())
            .collect::<Vec<_>>()
    };
    // Baseline: red then blue.
    assert_eq!(render(None, None), vec![0xF800, 0xF800, 0x001F, 0x001F]);
    // Horizontal flip swaps the halves.
    assert_eq!(
        render(Some("flip-h"), None),
        vec![0x001F, 0x001F, 0xF800, 0xF800]
    );
    // A 180° rotation on a 1-row image matches the horizontal flip.
    assert_eq!(
        render(None, Some(180)),
        vec![0x001F, 0x001F, 0xF800, 0xF800]
    );
}

#[test]
fn image_fit_preserves_aspect_ratio() {
    // N575: `fit` letterboxes a 1x1 source into a 4x1 rect (only the centre
    // column is drawn); stretching fills every column.
    use mdc_protocol::SceneNode;
    let render = |mode: &str, align: Option<&str>| {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 1,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "pic".into(),
            kind: "image".into(),
            rect: Some([0, 0, 4, 1]),
            value: Some("f800".into()),
            value2: Some("1,1".into()),
            children: Vec::new(),
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: align.map(str::to_string),
            z: None,
            wrap: None,
            mode: Some(mode.into()),
            focus: None,
        });
        assert!(device.render_scene());
        (0..4)
            .map(|x| device.pixel_rgb565(x, 0).unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(render("", None), vec![0xF800, 0xF800, 0xF800, 0xF800]);
    assert_eq!(render("fit", None), vec![0, 0xF800, 0, 0]);
    // N585: `align` places the letterbox left/right.
    assert_eq!(render("fit", Some("left")), vec![0xF800, 0, 0, 0]);
    assert_eq!(render("fit", Some("right")), vec![0, 0, 0, 0xF800]);
}

#[test]
fn image_fit_vertical_alignment() {
    // N635: `align` also carries the vertical axis (`top`/`middle`/`bottom`),
    // alone or comma-separated with a horizontal value.
    use mdc_protocol::SceneNode;
    let render = |align: Option<&str>| {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 1,
            height: 4,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "pic".into(),
            kind: "image".into(),
            rect: Some([0, 0, 1, 4]),
            value: Some("f800".into()),
            value2: Some("1,1".into()),
            children: Vec::new(),
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: align.map(str::to_string),
            z: None,
            wrap: None,
            mode: Some("fit".into()),
            focus: None,
        });
        assert!(device.render_scene());
        (0..4)
            .map(|y| device.pixel_rgb565(0, y).unwrap())
            .collect::<Vec<_>>()
    };
    // A 1x1 source in a 1x4 rect letterboxes to a single row.
    assert_eq!(render(None), vec![0, 0xF800, 0, 0], "default centred");
    assert_eq!(render(Some("top")), vec![0xF800, 0, 0, 0]);
    assert_eq!(render(Some("bottom")), vec![0, 0, 0, 0xF800]);
    assert_eq!(render(Some("middle")), vec![0, 0xF800, 0, 0]);
    // Comma-separated axes: horizontal value + vertical value.
    assert_eq!(render(Some("left,top")), vec![0xF800, 0, 0, 0]);
    assert_eq!(render(Some("center,bottom")), vec![0, 0, 0, 0xF800]);
    // An unknown token keeps the centred default.
    assert_eq!(render(Some("nope")), vec![0, 0xF800, 0, 0]);
}

#[test]
fn image_fit_pad_composes_with_rotate() {
    // N665: padding insets the box and the quarter-turn then fits inside it.
    use mdc_protocol::SceneNode;
    let lit = |rotate: Option<u16>| {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 4,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "pic".into(),
            kind: "image".into(),
            rect: Some([0, 0, 4, 4]),
            value: Some("f800001f".into()),
            value2: Some("2,1".into()),
            children: Vec::new(),
            opacity: None,
            visible: None,
            anim: None,
            rotate,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: Some("fit,pad=1".into()),
            focus: None,
        });
        assert!(device.render_scene());
        let mut pixels: Vec<(u16, u16, u16)> = Vec::new();
        for y in 0..4u16 {
            for x in 0..4u16 {
                let value = device.pixel_rgb565(x, y).unwrap();
                if value != 0 {
                    pixels.push((x, y, value));
                }
            }
        }
        pixels
    };
    // 2x1 source inside the 2x2 inner box: a 2x1 bar at row 1.
    let flat = lit(None);
    assert_eq!(flat.len(), 2);
    assert!(flat.iter().all(|(x, y, _)| *y == 1 && (1..=2).contains(x)));
    // Rotated 90°: a 1x2 column at column 1 (rows 1..=2).
    let turned = lit(Some(90));
    assert_eq!(turned.len(), 2);
    assert!(turned
        .iter()
        .all(|(x, y, _)| *x == 1 && (1..=2).contains(y)));
    // The two colours are preserved after the quarter-turn.
    assert_ne!(flat[0].2, flat[1].2);
    assert_ne!(turned[0].2, turned[1].2);
}

#[test]
fn image_fit_pad_composes_with_align() {
    // N675: pad insets the box, then align/valign place the fitted image.
    use mdc_protocol::SceneNode;
    let bounds = |src: &str, align: &str| {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 8,
            height: 8,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "pic".into(),
            kind: "image".into(),
            rect: Some([0, 0, 8, 8]),
            value: Some("f800001f".into()),
            value2: Some(src.into()),
            children: Vec::new(),
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: Some(align.into()),
            z: None,
            wrap: None,
            mode: Some("fit,pad=2".into()),
            focus: None,
        });
        assert!(device.render_scene());
        let mut min_x = u16::MAX;
        let mut min_y = u16::MAX;
        for y in 0..8u16 {
            for x in 0..8u16 {
                if device.pixel_rgb565(x, y).unwrap() != 0 {
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                }
            }
        }
        (min_x, min_y)
    };
    // A tall 1x2 source: only the horizontal align has slack.
    assert_eq!(bounds("1,2", "left"), (2, 2));
    assert_eq!(bounds("1,2", "center"), (3, 2));
    assert_eq!(bounds("1,2", "right"), (4, 2));
    // A wide 2x1 source: only the vertical align has slack.
    assert_eq!(bounds("2,1", "left,top"), (2, 2));
    assert_eq!(bounds("2,1", "left,middle"), (2, 3));
    assert_eq!(bounds("2,1", "left,bottom"), (2, 4));
}

#[test]
fn image_fit_padding_insets_the_letterbox() {
    // N645: `pad=N` insets the fit box on every side.
    use mdc_protocol::SceneNode;
    let render = |mode: &str| {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 4,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "pic".into(),
            kind: "image".into(),
            rect: Some([0, 0, 4, 4]),
            value: Some("f800".into()),
            value2: Some("1,1".into()),
            children: Vec::new(),
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
        });
        assert!(device.render_scene());
        let mut lit = Vec::new();
        for y in 0..4u16 {
            for x in 0..4u16 {
                if device.pixel_rgb565(x, y).unwrap() != 0 {
                    lit.push((x, y));
                }
            }
        }
        lit
    };
    // No padding: the 1x1 source fills the 4x4 rect (stretch) or, with fit,
    // becomes a 4x4 block.
    assert_eq!(render("fit").len(), 16);
    // pad=1 leaves a 2x2 inner box, which the square source fills.
    let padded = render("fit,pad=1");
    assert_eq!(padded.len(), 4, "2x2 inner box: {padded:?}");
    assert!(padded
        .iter()
        .all(|(x, y)| (1..=2).contains(x) && (1..=2).contains(y)));
    // pad=3 is clamped to 1 (the rect is 4x4).
    assert_eq!(render("fit,pad=3").len(), 4);
    // Padding does not apply to a stretch (no fit).
    assert_eq!(render("pad=1").len(), 16);
}

#[test]
fn image_fit_per_side_padding() {
    // N655: `pad-l/-t/-r/-b` inset one side each.
    use mdc_protocol::SceneNode;
    let render = |mode: &str| {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 5,
            height: 5,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "pic".into(),
            kind: "image".into(),
            rect: Some([0, 0, 5, 5]),
            value: Some("f800".into()),
            value2: Some("1,1".into()),
            children: Vec::new(),
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
        });
        assert!(device.render_scene());
        let mut lit = Vec::new();
        for y in 0..5u16 {
            for x in 0..5u16 {
                if device.pixel_rgb565(x, y).unwrap() != 0 {
                    lit.push((x, y));
                }
            }
        }
        let min_x = lit.iter().map(|(x, _)| *x).min().unwrap();
        let max_x = lit.iter().map(|(x, _)| *x).max().unwrap();
        let min_y = lit.iter().map(|(_, y)| *y).min().unwrap();
        let max_y = lit.iter().map(|(_, y)| *y).max().unwrap();
        (lit.len(), min_x, min_y, max_x, max_y)
    };
    // All sides: a 3x3 inner box, so a 1x1 source fills 9 pixels.
    assert_eq!(render("fit,pad=1"), (9, 1, 1, 3, 3));
    // Left only: 3 wide, full height -> 3x3, centred vertically.
    assert_eq!(render("fit,pad-l=2"), (9, 2, 1, 4, 3));
    // Top 1, bottom 2: inner height 2 -> a 2x2 block at x/y 1..2.
    assert_eq!(render("fit,pad-t=1,pad-b=2"), (4, 1, 1, 2, 2));
    // Right and bottom together: the box shrinks to 4x4 and the square source
    // fills it from the (unpadded) top-left corner.
    assert_eq!(render("fit,pad-r=1,pad-b=1"), (16, 0, 0, 3, 3));
    // A huge per-side value is clamped so the inner box stays non-empty.
    let (count, min_x, _, max_x, _) = render("fit,pad-l=99");
    assert!(count > 0 && max_x >= min_x);
}

#[test]
fn image_fit_composes_with_transforms() {
    // N595: `fit` composes with `flip-h`/`rotate` and `align`.
    use mdc_protocol::SceneNode;
    let render = |mode: &str, rotate: Option<u16>, align: Option<&str>| {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 1,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "pic".into(),
            kind: "image".into(),
            rect: Some([0, 0, 4, 1]),
            value: Some("f800001f".into()),
            value2: Some("2,1".into()),
            children: Vec::new(),
            opacity: None,
            visible: None,
            anim: None,
            rotate,
            blend: None,
            align: align.map(str::to_string),
            z: None,
            wrap: None,
            mode: Some(mode.into()),
            focus: None,
        });
        assert!(device.render_scene());
        (0..4)
            .map(|x| device.pixel_rgb565(x, 0).unwrap())
            .collect::<Vec<_>>()
    };
    // A 2x1 source fits 1:1 and is letterboxed centred.
    assert_eq!(render("fit", None, None), vec![0, 0xF800, 0x001F, 0]);
    // Flipping swaps the drawn halves in place.
    assert_eq!(render("fit,flip-h", None, None), vec![0, 0x001F, 0xF800, 0]);
    // A 180° rotation on a 1-row image matches the flip.
    assert_eq!(render("fit", Some(180), None), vec![0, 0x001F, 0xF800, 0]);
    // Left alignment moves the region before flipping.
    assert_eq!(
        render("fit,flip-h", None, Some("left")),
        vec![0x001F, 0xF800, 0, 0]
    );
}

#[test]
fn image_fit_quarter_turn_swaps_the_box() {
    // N605: a 1x2 source fitted into 4x4 is a tall 2x4 box; rotated 90° it
    // becomes a wide 4x2 box.
    use mdc_protocol::SceneNode;
    let render = |rotate: Option<u16>| {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 4,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "pic".into(),
            kind: "image".into(),
            rect: Some([0, 0, 4, 4]),
            value: Some("f800001f".into()),
            value2: Some("1,2".into()),
            children: Vec::new(),
            opacity: None,
            visible: None,
            anim: None,
            rotate,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: Some("fit".into()),
            focus: None,
        });
        assert!(device.render_scene());
        (0..4)
            .map(|y| {
                (0..4)
                    .map(|x| device.pixel_rgb565(x, y).unwrap())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    // Tall box: column 0 is outside (centred at column 1).
    let tall = render(None);
    assert_eq!(tall[0][0], 0, "column 0 is outside the tall box");
    assert_ne!(tall[0][1], 0, "column 1 is inside");
    // Wide box: row 0 is outside (centred at row 1).
    let wide = render(Some(90));
    assert_eq!(wide[0][0], 0, "row 0 is outside the wide box");
    assert_ne!(wide[1][0], 0, "row 1 is inside");
}

#[test]
fn image_fit_transform_matrix_identities() {
    // N615: fit composes with the flip/rotate matrix; a 180° turn equals both
    // flips, and the quarter turns differ for an asymmetric source.
    use mdc_protocol::SceneNode;
    let render = |mode: &str, rotate: Option<u16>| {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 4,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "pic".into(),
            kind: "image".into(),
            rect: Some([0, 0, 4, 4]),
            // 2x2 source: red, green, blue, white (top-left..bottom-right).
            value: Some("f80007e0001fffff".into()),
            value2: Some("2,2".into()),
            children: Vec::new(),
            opacity: None,
            visible: None,
            anim: None,
            rotate,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: Some(mode.into()),
            focus: None,
        });
        assert!(device.render_scene());
        (0..4)
            .flat_map(|y| (0..4).map(move |x| (x, y)))
            .map(|(x, y)| device.pixel_rgb565(x, y).unwrap())
            .collect::<Vec<_>>()
    };
    let base = render("fit", None);
    // 180° == horizontal flip + vertical flip.
    assert_eq!(render("fit", Some(180)), render("fit,flip-h,flip-v", None));
    // The two quarter turns differ, and neither is the identity.
    let cw = render("fit", Some(90));
    let ccw = render("fit", Some(270));
    assert_ne!(cw, ccw);
    assert_ne!(cw, base);
    assert_ne!(ccw, base);
}

#[test]
fn image_fit_align_with_rotate90() {
    // N625: `align` positions the rotated letterbox (a 2x3 source in 8x4,
    // turned 90°, becomes a 6x4 box with 2 columns of slack).
    use mdc_protocol::SceneNode;
    let render = |align: &str| {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 8,
            height: 4,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "pic".into(),
            kind: "image".into(),
            rect: Some([0, 0, 8, 4]),
            value: Some("f80007e0001ffffff81f07ff".into()),
            value2: Some("2,3".into()),
            children: Vec::new(),
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
        });
        assert!(device.render_scene());
        // The leftmost non-empty column over the whole frame.
        let grid: Vec<Vec<u16>> = (0..4)
            .map(|y| (0..8).map(|x| device.pixel_rgb565(x, y).unwrap()).collect())
            .collect();
        let mut leftmost = None;
        for (x, _) in (0..8).enumerate() {
            if grid.iter().any(|row| row[x] != 0) {
                leftmost = Some(x);
                break;
            }
        }
        (grid, leftmost)
    };
    let (_, left) = render("left");
    let (_, center) = render("center");
    let (_, right) = render("right");
    assert_eq!(left, Some(0), "left starts at column 0");
    assert_eq!(center, Some(1), "centre starts at column 1");
    assert_eq!(right, Some(2), "right starts at column 2");
}

#[test]
fn glyphs_cover_digits_letters_and_punctuation() {
    for ch in ".,:!?-+/()=*#".bytes() {
        assert!(glyph_columns(ch).is_some(), "missing {}", ch as char);
    }
    // N476: lowercase has its own glyph (not the uppercase one).
    assert_ne!(glyph_columns(b'a'), glyph_columns(b'A'));
    assert!(glyph_columns(b'z').is_some());
    assert_eq!(glyph_columns(b' '), Some([0, 0, 0, 0, 0]));
    // N494: the extended punctuation renders too.
    for ch in "\"'%&@;<>~".bytes() {
        assert!(glyph_columns(ch).is_some(), "missing {}", ch as char);
    }
}

#[test]
fn scene_text_scales_with_the_rect() {
    use mdc_protocol::SceneNode;
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 24,
        height: 16,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

    let text = |rect: [u16; 4]| SceneNode {
        name: "label".into(),
        kind: "text".into(),
        rect: Some(rect),
        value: Some("H".into()),
        children: Vec::new(),
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
    // rect 0×0 -> scale 1, so (1,1) stays clear on the fresh framebuffer.
    session.set_scene(text([0, 0, 0, 0])).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_scene());
    assert_eq!(
        device.pixel_rgb565(1, 1),
        Some(0),
        "scale 1 leaves (1,1) clear"
    );

    // rect 12×14 -> 2× glyph scale, so the scaled pixel lights up.
    session.set_scene(text([0, 0, 12, 14])).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_scene());
    assert_ne!(device.pixel_rgb565(1, 1), Some(0), "scale 2 fills (1,1)");
}

#[test]
fn text_wraps_to_the_rect_width() {
    assert_eq!(
        wrap_text("ab\ncd", 10),
        vec!["ab".to_string(), "cd".to_string()]
    );
    assert_eq!(
        wrap_text("hello world", 7),
        vec!["hello".to_string(), "world".to_string()]
    );
    // A word longer than the line is hard-broken.
    assert_eq!(
        wrap_text("abcdef", 3),
        vec!["abc".to_string(), "def".to_string()]
    );
    // A blank paragraph keeps a blank line.
    assert_eq!(
        wrap_text("a\n\nb", 5),
        vec!["a".to_string(), "".to_string(), "b".to_string()]
    );
    // Zero width still makes progress one char per line.
    assert_eq!(wrap_text("ab", 0), vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn scene_text_draws_wrapped_lines() {
    use mdc_protocol::SceneNode;
    // 40×30 box, 30-wide text rect -> scale 1, 5 columns; two lines.
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 40,
        height: 30,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    let scene = SceneNode {
        name: "label".into(),
        kind: "text".into(),
        rect: Some([0, 0, 30, 7]),
        value: Some("hello world".into()),
        children: Vec::new(),
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
    // Line 1 at y=0, line 2 at y=8 (line height 8 at scale 1).
    // N476: pixel positions depend on the glyph, so scan each line's band.
    let line_lit =
        |y0: u16, y1: u16| (y0..y1).any(|y| (0..30).any(|x| device.pixel_rgb565(x, y) != Some(0)));
    assert!(line_lit(0, 7), "first line drawn");
    assert!(line_lit(8, 15), "wrapped line drawn");
    // Row 7 is the gap between the two lines and stays clear.
    assert_eq!(device.pixel_rgb565(0, 7), Some(0), "line gap is clear");
}

#[test]
fn scene_rect_opacity_blends_over_the_background() {
    use mdc_protocol::SceneNode;
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 4,
        height: 4,
        ..BoardProfile::default()
    });
    let rect = |name: &str, color: &str, opacity: Option<u8>| SceneNode {
        name: name.into(),
        kind: "rect".into(),
        rect: Some([0, 0, 4, 4]),
        value: Some(color.into()),
        opacity,
        value2: None,
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
    };
    device.scene = Some(SceneNode {
        name: "root".into(),
        kind: "group".into(),
        rect: None,
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
        children: vec![
            rect("bg", "#FFFFFF", None),
            rect("fg", "#FF0000", Some(128)),
        ],
        value2: None,
    });
    assert!(device.render_scene());
    let px = device.pixel_rgb565(1, 1).unwrap();
    // White blended half-way to red: not pure red, not white.
    assert_ne!(px, 0xF800);
    assert_ne!(px, 0xFFFF);
    assert!((px >> 11) > 20, "red channel is high: {px:#06x}");
}

#[test]
fn surface_weights_config_is_validated_and_survives_reload() {
    // N777: the device stores a bounded weight list; bad values are rejected
    // without changing the previous one, and the host can read it back.
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 32,
        height: 32,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    session.set_config("surface_weights", "3,1").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(session.surface_weights(), Some(vec![3, 1]));
    assert_eq!(
        session
            .plan_shared_surface_tiles_from_config()
            .unwrap()
            .surfaces
            .len(),
        1,
        "the default board exposes one surface"
    );
    for bad in ["0,1", "1,2,3,4,5", "1,x", ""] {
        session.set_config("surface_weights", bad).unwrap();
        device.poll().unwrap();
        let _ = session.poll();
    }
    assert_eq!(
        session.surface_weights(),
        Some(vec![3, 1]),
        "invalid weights leave the old value"
    );
}

#[test]
fn device_filtering_config_sets_the_sampler() {
    let (host, mut device) = FakeDevice::pair(BoardProfile::default());
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.texture_filter, mdc_bake::Filter::Nearest);
    session.set_config("filtering", "linear").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.texture_filter, mdc_bake::Filter::Linear);
    session.set_config("filtering", "bogus").unwrap();
    device.poll().unwrap();
    let _ = session.poll();
    assert_eq!(
        device.texture_filter,
        mdc_bake::Filter::Linear,
        "an unknown filter leaves the sampler unchanged"
    );
}

#[test]
fn board_config_defaults_apply() {
    // N596: a board ships its own default for a managed key.
    let query = |board: &str| {
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
    assert_eq!(query("default").as_deref(), Some("0"));
    assert_eq!(query("linux-virt").as_deref(), Some("2"));
}

#[test]
fn scene_gradient_interpolates_top_to_bottom() {
    use mdc_protocol::SceneNode;
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 1,
        height: 8,
        ..BoardProfile::default()
    });
    device.scene = Some(SceneNode {
        name: "g".into(),
        kind: "gradient".into(),
        rect: Some([0, 0, 1, 8]),
        value: Some("#FF0000".into()),
        value2: Some("#0000FF".into()),
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
    });
    assert!(device.render_scene());
    assert_eq!(device.pixel_rgb565(0, 0), Some(0xF800), "top is red");
    assert_eq!(device.pixel_rgb565(0, 7), Some(0x001F), "bottom is blue");
    let mid = device.pixel_rgb565(0, 4).unwrap();
    assert!(
        (mid >> 11) > 0 && (mid & 0x1f) > 0,
        "mid blended: {mid:#06x}"
    );
}

#[test]
fn eink_full_refresh_clears_ghosting() {
    // N499: repeated partial refreshes fade the ghost; a full frame is exact.
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 2,
        height: 2,
        color_fps: false,
        supports_partial_update: true,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    let frame = |bytes: Vec<u8>| mdc_core::Frame {
        surface_id: "main".into(),
        width: 2,
        height: 2,
        bytes,
    };
    // Start from a clean black panel.
    session.send_frame(frame(vec![0u8; 8])).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.pixel_rgb565(0, 0).unwrap(), 0);
    let white_tile = |base: u64| mdc_core::Tile {
        surface_id: "main".into(),
        base_frame_id: base,
        x: 0,
        y: 0,
        width: 1,
        height: 1,
        bytes: vec![0xff, 0xff],
    };
    // A white partial refresh leaves a ghost...
    device.advance(200).unwrap();
    let base = device.current_frame_id();
    session.send_tile(white_tile(base)).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    let after_first = device.pixel_rgb565(0, 0).unwrap();
    assert!(
        after_first != 0 && after_first != 0xffff,
        "partial refresh ghosts: {after_first:#06x}"
    );
    // ...that fades with a second identical refresh.
    device.advance(200).unwrap();
    session.send_tile(white_tile(base)).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    let after_second = device.pixel_rgb565(0, 0).unwrap();
    assert!(
        after_second > after_first,
        "the ghost fades toward the target ({after_first:#06x} -> {after_second:#06x})"
    );
    // A full white frame clears it exactly.
    device.advance(200).unwrap();
    session.send_frame(frame(vec![0xff; 8])).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(
        device.pixel_rgb565(0, 0).unwrap(),
        0xffff,
        "a full refresh is exact"
    );
}

#[test]
fn eink_tiles_leave_a_ghost() {
    // N489: a white tile over black is not fully white on e-ink.
    let apply = |board: &str| {
        let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id(board));
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
        let _ = session.send_tile(mdc_core::Tile {
            surface_id: "main".into(),
            base_frame_id: device.current_frame_id(),
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            bytes: vec![0xff, 0xff],
        });
        device.poll().unwrap();
        session.poll().unwrap();
        device.pixel_rgb565(0, 0).unwrap()
    };
    let color = apply("linux-virt");
    assert_eq!(color, 0xffff, "a colour panel replaces the pixel");
    let eink = apply("waveshare-eink");
    assert!(
        eink != 0 && eink != 0xffff,
        "e-ink keeps a ghost: {eink:#06x}"
    );
}

#[test]
fn z_order_overrides_document_order() {
    // N485: a higher `z` draws on top regardless of document order.
    use mdc_protocol::SceneNode;
    let render = |green_z: Option<i32>| {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 4,
            ..BoardProfile::default()
        });
        let rect = |name: &str, color: &str, z: Option<i32>| SceneNode {
            name: name.into(),
            kind: "rect".into(),
            rect: Some([0, 0, 4, 4]),
            value: Some(color.into()),
            value2: None,
            opacity: Some(255),
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
        device.scene = Some(SceneNode {
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
                rect("green", "#00FF00", green_z),
                rect("red", "#FF0000", None),
            ],
        });
        assert!(device.render_scene());
        device.pixel_rgb565(1, 1).unwrap()
    };
    // Document order: red (last) wins.
    let red = render(None);
    assert!(
        red >> 11 > 8 && ((red >> 5) & 0x3f) < 8,
        "red on top: {red:#06x}"
    );
    // z=1 lifts green above red, and hit-testing agrees.
    let green = render(Some(1));
    assert!(
        ((green >> 5) & 0x3f) > 8 && green >> 11 < 8,
        "green on top: {green:#06x}"
    );
}

#[test]
fn radial_focus_moves_the_bright_centre() {
    // N535: `focus` places the radial centre at a fraction of the rect.
    use mdc_protocol::SceneNode;
    let render = |focus: Option<&str>| {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 9,
            height: 9,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "spot".into(),
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
            focus: focus.map(str::to_string),
            children: Vec::new(),
        });
        assert!(device.render_scene());
        let at = |x: u16, y: u16| device.pixel_rgb565(x, y).unwrap();
        (at(0, 0), at(4, 4), at(8, 8))
    };
    // Default: the middle is brightest.
    let (top_left, centre, bottom_right) = render(None);
    assert!(centre > top_left && centre > bottom_right, "centred");
    // focus 0,0: the top-left corner is the bright one.
    let (top_left, centre, _) = render(Some("0,0"));
    assert!(
        top_left > centre,
        "focused top-left: {top_left:#06x} vs {centre:#06x}"
    );
    // focus 100,100: the bottom-right corner is the bright one.
    let (_, centre, bottom_right) = render(Some("100,100"));
    assert!(bottom_right > centre, "focused bottom-right");
    // An unparsable focus falls back to centred.
    let (top_left, centre, _) = render(Some("nope"));
    assert!(centre > top_left, "fallback is centred");
}

#[test]
fn focus_animation_moves_the_radial_centre_over_time() {
    // N546: a focus keyframe track animates the radial centre on the clock.
    use mdc_protocol::{SceneAnim, SceneNode};
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 9,
        height: 9,
        ..BoardProfile::default()
    });
    device.scene = Some(SceneNode {
        name: "spot".into(),
        kind: "gradient".into(),
        rect: Some([0, 0, 9, 9]),
        value: Some("#FFFFFF".into()),
        value2: Some("#000000".into()),
        opacity: None,
        visible: None,
        anim: Some(SceneAnim {
            duration_ms: 100,
            looping: false,
            opacity: Vec::new(),
            rect: Vec::new(),
            // Bright focus starts top-left, ends bottom-right.
            focus: vec![[0, 0, 0], [100, 100, 100]],
            delay_ms: None,
            speed_permille: None,
            color: Vec::new(),
            rotate: Vec::new(),
            easing: None,
            scale: Vec::new(),
            offset: Vec::new(),
            frame: Vec::new(),
        }),
        rotate: None,
        blend: None,
        align: None,
        z: None,
        wrap: None,
        mode: Some("radial".into()),
        focus: Some("0,0".into()),
        children: Vec::new(),
    });
    let top_left_bright = |device: &FakeDevice| {
        let at = |x: u16, y: u16| device.pixel_rgb565(x, y).unwrap();
        at(0, 0) > at(8, 8)
    };
    device.clock.tick = 0;
    assert!(device.render_scene());
    assert!(top_left_bright(&device), "t=0 focuses the top-left");
    device.clock.tick = 100;
    assert!(device.render_scene());
    assert!(!top_left_bright(&device), "t=100 focuses the bottom-right");
}

#[test]
fn gradient_angle_controls_the_direction() {
    // N525: `rotate` sets the ramp direction; 180/270 reverse it and other
    // angles give a diagonal.
    use mdc_protocol::SceneNode;
    let render = |rotate: Option<u16>| {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 8,
            height: 8,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "ramp".into(),
            kind: "gradient".into(),
            rect: Some([0, 0, 8, 8]),
            value: Some("#FFFFFF".into()),
            value2: Some("#000000".into()),
            opacity: None,
            visible: None,
            anim: None,
            rotate,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            children: Vec::new(),
        });
        assert!(device.render_scene());
        let mut pixels = Vec::new();
        for y in 0..8u16 {
            for x in 0..8u16 {
                pixels.push(device.pixel_rgb565(x, y).unwrap());
            }
        }
        pixels
    };
    let at = |pixels: &[u16], x: usize, y: usize| pixels[y * 8 + x];
    // 0: top bright, bottom dark.
    let up = render(None);
    assert!(at(&up, 0, 0) > at(&up, 0, 7));
    assert_eq!(at(&up, 0, 0), 0xFFFF);
    // 180: reversed.
    let down = render(Some(180));
    assert!(at(&down, 0, 7) > at(&down, 0, 0));
    assert_eq!(at(&down, 0, 7), 0xFFFF);
    // 90: left bright, right dark; 270 reverses it.
    let right = render(Some(90));
    assert!(at(&right, 0, 0) > at(&right, 7, 0));
    let left = render(Some(270));
    assert!(at(&left, 7, 0) > at(&left, 0, 0));
    // 45: the diagonal runs from the top-left to the bottom-right.
    let diagonal = render(Some(45));
    assert!(at(&diagonal, 0, 0) > at(&diagonal, 7, 7));
    assert!(at(&diagonal, 0, 0) > at(&diagonal, 7, 0), "corner vs side");
    // 135: the other diagonal (bright at the bottom-left).
    let other = render(Some(135));
    assert!(at(&other, 0, 7) > at(&other, 7, 0));
}

#[test]
fn radial_gradients_are_brighter_at_the_centre() {
    // N515: a radial gradient is brightest (the start colour) at the centre;
    // linear mode still ramps top-to-bottom.
    use mdc_protocol::SceneNode;
    let render = |width: u16, height: u16, mode: Option<&str>| {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width,
            height,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "glow".into(),
            kind: "gradient".into(),
            rect: Some([0, 0, width, height]),
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
            mode: mode.map(str::to_string),
            focus: None,
            children: Vec::new(),
        });
        assert!(device.render_scene());
        let mut pixels = Vec::new();
        for y in 0..height {
            for x in 0..width {
                pixels.push(device.pixel_rgb565(x, y).unwrap());
            }
        }
        pixels
    };
    // Radial: the centre is the start colour, the corner much darker.
    let radial = render(9, 9, Some("radial"));
    let centre = radial[4 * 9 + 4];
    let corner = radial[0];
    assert!(centre > 0xC000, "bright centre: {centre:#06x}");
    assert!(centre > corner, "centre brighter than corner");
    // Linear: the top is bright and the bottom dark.
    let linear = render(1, 9, None);
    assert!(linear[0] > linear[8], "linear ramps down");
    assert_eq!(linear[0], 0xFFFF);
}

#[test]
fn multi_stop_gradients_blend_through_each_stop() {
    // N505: `value2` as `offset:#colour` stops.
    use mdc_protocol::SceneNode;
    let render = |value2: &str| {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 1,
            height: 11,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "ramp".into(),
            kind: "gradient".into(),
            rect: Some([0, 0, 1, 11]),
            value: Some("#FF0000".into()),
            value2: Some(value2.into()),
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
        });
        assert!(device.render_scene());
        (0..11u16)
            .map(|y| device.pixel_rgb565(0, y).unwrap())
            .collect::<Vec<_>>()
    };
    let stops = render("0:#FF0000,50:#00FF00,100:#0000FF");
    let channel = |value: u16, shift: u32, mask: u16| (value >> shift) & mask;
    // Top is red, the middle is green, and the bottom is blue.
    assert!(
        channel(stops[0], 11, 0x1f) > 24,
        "red top: {:#06x}",
        stops[0]
    );
    assert!(channel(stops[0], 5, 0x3f) < 8, "no green at the top");
    assert!(
        channel(stops[5], 5, 0x3f) > 40,
        "green middle: {:#06x}",
        stops[5]
    );
    assert!(channel(stops[5], 11, 0x1f) < 12 && channel(stops[5], 0, 0x1f) < 12);
    assert!(
        channel(stops[10], 0, 0x1f) > 24,
        "blue bottom: {:#06x}",
        stops[10]
    );
    // A two-stop `value`/`value2` ramp is unchanged (red -> blue).
    let ramp = render("#0000FF");
    assert_eq!(ramp[0], mdc_protocol::parse_color("#FF0000"));
    assert_eq!(ramp[10], mdc_protocol::parse_color("#0000FF"));
}

#[test]
fn dashed_lines_leave_gaps() {
    // N497: a dash length paints the first pixels of each period.
    use mdc_protocol::SceneNode;
    let lit = |dash: Option<&str>| {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 16,
            height: 1,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "rule".into(),
            kind: "line".into(),
            rect: Some([0, 0, 15, 0]),
            value: Some("#FFFFFF".into()),
            value2: dash.map(str::to_string),
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
        });
        assert!(device.render_scene());
        (0..16u16)
            .map(|x| device.pixel_rgb565(x, 0).unwrap() != 0)
            .collect::<Vec<_>>()
    };
    let solid = lit(None);
    assert!(solid.iter().all(|v| *v), "a solid line paints every pixel");
    let dashed = lit(Some("2"));
    assert_eq!(
        dashed,
        vec![
            true, true, false, false, true, true, false, false, true, true, false, false, true,
            true, false, false
        ],
        "dash length 2 leaves 2-pixel gaps"
    );
}

#[test]
fn text_wrap_mode_selects_line_breaking() {
    // N495: `word`/`char` wrap to the rect width; `none` keeps one line.
    use mdc_protocol::SceneNode;
    let lit_rows = |wrap: Option<&str>| {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 18,
            height: 16,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "label".into(),
            kind: "text".into(),
            rect: Some([0, 0, 18, 7]),
            value: Some("ab cd".into()),
            value2: None,
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: wrap.map(str::to_string),
            mode: None,
            focus: None,
            children: Vec::new(),
        });
        assert!(device.render_scene());
        let mut rows = Vec::new();
        for y in 0..16u16 {
            let lit = (0..18u16).any(|x| device.pixel_rgb565(x, y).unwrap() != 0);
            rows.push(lit);
        }
        rows
    };
    // The default (word) and char modes break the phrase onto two lines.
    for mode in [None, Some("word"), Some("char")] {
        let rows = lit_rows(mode);
        assert!(rows[0], "first line drawn ({mode:?})");
        assert!(
            rows.iter().skip(8).any(|v| *v),
            "wrapped onto a second line ({mode:?})"
        );
    }
    // `none` draws a single line, so nothing lands on the second line.
    let none = lit_rows(Some("none"));
    assert!(none[0]);
    assert!(
        !none.iter().skip(8).any(|v| *v),
        "no wrap keeps a single line"
    );
}

#[test]
fn text_align_shifts_lines_within_the_rect() {
    // N484: centre alignment moves the glyphs right of the rect origin.
    use mdc_protocol::SceneNode;
    let render = |align: Option<&str>| {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 24,
            height: 8,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "label".into(),
            kind: "text".into(),
            rect: Some([0, 0, 24, 7]),
            value: Some("hi".into()),
            value2: None,
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: align.map(str::to_string),
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            children: Vec::new(),
        });
        assert!(device.render_scene());
        let mut row = Vec::new();
        for x in 0..24u16 {
            row.push(device.pixel_rgb565(x, 0).unwrap());
        }
        row
    };
    let left = render(None);
    let center = render(Some("center"));
    assert_ne!(left, center, "centred text differs");
    let first_lit = |row: &[u16]| row.iter().position(|v| *v != 0).unwrap_or(usize::MAX);
    assert!(
        first_lit(&center) > first_lit(&left),
        "centre alignment starts further right"
    );
}

#[test]
fn text_ellipsis_and_vertical_alignment() {
    // N740: over-long lines get `...`; `valign=bottom` moves the glyph block
    // down inside a taller rect.
    assert_eq!(truncate_ellipsis("abcdefgh", 8), "abcdefgh");
    assert_eq!(truncate_ellipsis("abcdefghij", 8), "abcde...");
    assert_eq!(truncate_ellipsis("abcdefghij", 2), "ab");

    use mdc_protocol::SceneNode;
    let lit_rows = |mode: Option<&str>| {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 32,
            height: 20,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "label".into(),
            kind: "text".into(),
            rect: Some([0, 0, 32, 20]),
            value: Some("I".into()),
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
            children: Vec::new(),
        });
        assert!(device.render_scene());
        (0..20u16)
            .filter(|y| (0..32u16).any(|x| device.pixel_rgb565(x, *y).unwrap() != 0))
            .collect::<Vec<_>>()
    };
    let top = lit_rows(Some("valign=top"));
    let bottom = lit_rows(Some("valign=bottom"));
    assert_eq!(top.first(), Some(&0));
    assert_eq!(bottom.first(), Some(&6), "block height 14 in a 20px rect");
    assert!(bottom.iter().all(|y| *y >= 6));
}

#[test]
fn scene_draws_path_command_streams() {
    // N743: M/L/Z draws a closed triangle; Q bends between its endpoints.
    use mdc_protocol::SceneNode;
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 24,
        height: 24,
        ..BoardProfile::default()
    });
    let node = |value2: &str| SceneNode {
        name: "path".into(),
        kind: "path".into(),
        rect: Some([0, 0, 24, 24]),
        children: Vec::new(),
        value: Some("#0000FF".into()),
        value2: Some(value2.into()),
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
    };
    device.scene = Some(node("M2,20;L8,20;L8,14;Z"));
    assert!(device.render_scene());
    assert_eq!(device.pixel_rgb565(2, 20), Some(0x001F));
    assert_eq!(device.pixel_rgb565(8, 20), Some(0x001F));
    assert_eq!(device.pixel_rgb565(8, 14), Some(0x001F));
    assert_eq!(device.pixel_rgb565(5, 10), Some(0));

    device.scene = Some(node("M2,2;Q12,2 12,12"));
    assert!(device.render_scene());
    assert_eq!(device.pixel_rgb565(12, 12), Some(0x001F));
    assert_eq!(device.pixel_rgb565(10, 5), Some(0x001F));
}

#[test]
fn scene_mask_clips_children_to_a_polygon() {
    // N744: the child fills the whole rect but only the mask triangle shows.
    use mdc_protocol::SceneNode;
    let child = SceneNode {
        name: "fill".into(),
        kind: "rect".into(),
        rect: Some([0, 0, 16, 16]),
        children: Vec::new(),
        value: Some("#FF0000".into()),
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
    };
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 16,
        height: 16,
        ..BoardProfile::default()
    });
    device.scene = Some(SceneNode {
        name: "mask".into(),
        kind: "mask".into(),
        rect: Some([0, 0, 16, 16]),
        children: vec![child],
        value: None,
        value2: Some("1,1;14,1;1,14".into()),
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
    });
    assert!(device.render_scene());
    assert_eq!(device.pixel_rgb565(2, 2), Some(0xF800));
    assert_eq!(device.pixel_rgb565(12, 12), Some(0));
    // An out-of-bounds polygon is clipped, not a panic.
    if let Some(scene) = device.scene.as_mut() {
        scene.value2 = Some("-5,-5;20,-5;-5,20".into());
    }
    assert!(device.render_scene());
}

#[test]
fn text_background_and_underline_draw_bands() {
    // N751: `bg=` fills the line box and `underline` strokes below the glyphs.
    use mdc_protocol::SceneNode;
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 32,
        height: 20,
        ..BoardProfile::default()
    });
    device.scene = Some(SceneNode {
        name: "label".into(),
        kind: "text".into(),
        rect: Some([0, 0, 32, 20]),
        value: Some("I".into()),
        value2: None,
        opacity: None,
        visible: None,
        anim: None,
        rotate: None,
        blend: None,
        align: None,
        z: None,
        wrap: None,
        mode: Some("bg=#FF0000,underline".into()),
        focus: None,
        children: Vec::new(),
    });
    assert!(device.render_scene());
    assert_eq!(device.pixel_rgb565(20, 0), Some(0xF800), "line background");
    assert_eq!(device.pixel_rgb565(20, 10), Some(0xF800));
    assert_eq!(device.pixel_rgb565(5, 14), Some(0xFFFF), "underline");
}

#[test]
fn additive_blend_brightens_an_overlap() {
    // N480: an additive overlay adds to the base instead of covering it.
    use mdc_protocol::SceneNode;
    let render = |blend: Option<&str>| {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 4,
            ..BoardProfile::default()
        });
        let rect = |name: &str, color: &str, blend: Option<&str>| SceneNode {
            name: name.into(),
            kind: "rect".into(),
            rect: Some([0, 0, 4, 4]),
            value: Some(color.into()),
            value2: None,
            opacity: Some(128),
            visible: None,
            anim: None,
            rotate: None,
            blend: blend.map(str::to_string),
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            children: Vec::new(),
        };
        device.scene = Some(SceneNode {
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
                rect("base", "#00FF00", None),
                rect("glow", "#FF0000", blend),
            ],
        });
        assert!(device.render_scene());
        device.pixel_rgb565(1, 1).unwrap()
    };
    let over = render(None);
    let additive = render(Some("additive"));
    // Source-over fades green toward red; additive keeps the green and adds red.
    assert!(
        ((additive >> 5) & 0x3f) > ((over >> 5) & 0x3f),
        "green is preserved: over={over:#06x} additive={additive:#06x}"
    );
    // Additive keeps the base green and adds red, so it is strictly brighter.
    let brightness = |value: u16| {
        u32::from((value >> 11) & 0x1f) + u32::from((value >> 5) & 0x3f) + u32::from(value & 0x1f)
    };
    assert!(
        brightness(additive) > brightness(over),
        "additive is brighter: over={over:#06x} additive={additive:#06x}"
    );
}

#[test]
fn lowercase_glyphs_are_distinct() {
    // N476: `a` and `A` are different glyphs.
    let lower = glyph_columns(b'a').unwrap();
    let upper = glyph_columns(b'A').unwrap();
    assert_ne!(lower, upper);
    // Every letter renders (no gaps).
    for ch in b'a'..=b'z' {
        assert!(glyph_columns(ch).is_some(), "missing {ch}");
    }
    // Rendering "abc" differs from the uppercase form.
    let (mut a, mut b) = (vec![0u8; 32 * 2], vec![0u8; 32 * 2]);
    draw_text(&mut a, 16, 8, 0, 0, "abc", 0xFFFF, 1, 1, 255);
    draw_text(&mut b, 16, 8, 0, 0, "ABC", 0xFFFF, 1, 1, 255);
    assert_ne!(a, b);
    assert!(a.iter().any(|byte| *byte != 0), "lowercase text draws");
}

#[test]
fn scene_rotate_swaps_the_rect_axes() {
    // N475: rotating a wide rect by 90 degrees draws it tall.
    use mdc_protocol::SceneNode;
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 8,
        height: 8,
        ..BoardProfile::default()
    });
    device.scene = Some(SceneNode {
        name: "wide".into(),
        kind: "rect".into(),
        rect: Some([0, 0, 8, 2]),
        value: Some("#FFFFFF".into()),
        value2: None,
        opacity: None,
        visible: None,
        anim: None,
        rotate: Some(90),
        blend: None,
        align: None,
        z: None,
        wrap: None,
        mode: None,
        focus: None,
        children: Vec::new(),
    });
    assert!(device.render_scene());
    assert_eq!(device.pixel_rgb565(0, 0), Some(0xFFFF));
    // Rotated: 2 wide x 8 tall, so (1,7) is filled and (7,1) is not.
    assert_eq!(device.pixel_rgb565(1, 7), Some(0xFFFF), "tall after rotate");
    assert_eq!(device.pixel_rgb565(7, 1), Some(0), "not wide after rotate");
    assert_eq!(device.hit_test(1, 7).as_deref(), Some("wide"));
}

#[test]
fn overlay_surface_renders_independently() {
    // N465: the overlay has its own buffer and rasterises the scene there.
    use mdc_protocol::SceneNode;
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 4,
        height: 4,
        overlay_width: 2,
        overlay_height: 2,
        ..BoardProfile::default()
    });
    // No scene: a white placeholder fills the overlay, main stays untouched.
    assert!(device.render_overlay());
    assert_eq!(device.overlay_pixel_rgb565(0, 0), Some(0xFFFF));
    assert_eq!(device.pixel_rgb565(0, 0), Some(0), "main untouched");

    // A scene renders into the overlay at its own size.
    device.scene = Some(SceneNode {
        name: "o".into(),
        kind: "rect".into(),
        rect: Some([0, 0, 2, 2]),
        value: Some("#00FF00".into()),
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
        children: Vec::new(),
    });
    assert!(device.render_overlay());
    assert_eq!(device.overlay_pixel_rgb565(1, 1), Some(0x07E0));
}

#[test]
fn scene_animation_samples_on_the_device_clock() {
    // N463: opacity keyframes drive the rendered alpha over time.
    use mdc_protocol::{SceneAnim, SceneNode};
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 4,
        height: 4,
        ..BoardProfile::default()
    });
    device.scene = Some(SceneNode {
        name: "fade".into(),
        kind: "rect".into(),
        rect: Some([0, 0, 4, 4]),
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
            delay_ms: None,
            speed_permille: None,
            color: Vec::new(),
            rotate: Vec::new(),
            easing: None,
            scale: Vec::new(),
            offset: Vec::new(),
            frame: Vec::new(),
        }),
        rotate: None,
        blend: None,
        align: None,
        z: None,
        wrap: None,
        mode: None,
        focus: None,
        children: Vec::new(),
    });
    // t=0: fully transparent, so the black background remains.
    assert!(device.render_scene());
    assert_eq!(device.pixel_rgb565(0, 0), Some(0), "transparent at t=0");
    // t=duration: fully opaque white.
    device.clock.advance(100);
    assert!(device.render_scene());
    assert_eq!(device.pixel_rgb565(0, 0), Some(0xFFFF), "opaque at the end");
    // Midway: a blend between the two.
    device.clock.tick = 50;
    assert!(device.render_scene());
    let mid = device.pixel_rgb565(0, 0).unwrap();
    assert!(mid != 0 && mid != 0xFFFF, "blended midway: {mid:#06x}");
}

#[test]
fn scene_animation_delay_and_speed_reach_the_device() {
    // N823/N824: the device clock feeds delay and speed before sampling.
    use mdc_protocol::{SceneAnim, SceneNode};
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 2,
        height: 2,
        ..BoardProfile::default()
    });
    device.scene = Some(SceneNode {
        name: "fade".into(),
        kind: "rect".into(),
        rect: Some([0, 0, 2, 2]),
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
            delay_ms: Some(100),
            speed_permille: Some(2000),
            color: Vec::new(),
            rotate: Vec::new(),
            easing: None,
            scale: Vec::new(),
            offset: Vec::new(),
            frame: Vec::new(),
        }),
        rotate: None,
        blend: None,
        align: None,
        z: None,
        wrap: None,
        mode: None,
        focus: None,
        children: Vec::new(),
    });
    assert!(device.render_scene());
    assert_eq!(
        device.pixel_rgb565(1, 1),
        Some(0),
        "delay holds the first key"
    );
    device.clock.tick = 100;
    assert!(device.render_scene());
    assert_eq!(
        device.pixel_rgb565(1, 1),
        Some(0),
        "elapsed time is still 0"
    );
    device.clock.tick = 150;
    assert!(device.render_scene());
    assert_eq!(
        device.pixel_rgb565(1, 1),
        Some(0xFFFF),
        "50 ms at 2x reaches the end"
    );
}

#[test]
fn scene_colour_animation_changes_a_rect_over_time() {
    // N825/N826: colour keys interpolate and drive the rect fill.
    use mdc_protocol::{SceneAnim, SceneColorKey, SceneNode};
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 2,
        height: 2,
        ..BoardProfile::default()
    });
    device.scene = Some(SceneNode {
        name: "pulse".into(),
        kind: "rect".into(),
        rect: Some([0, 0, 2, 2]),
        value: Some("#000000".into()),
        value2: None,
        opacity: None,
        visible: None,
        anim: Some(SceneAnim {
            duration_ms: 100,
            looping: false,
            opacity: Vec::new(),
            rect: Vec::new(),
            focus: Vec::new(),
            delay_ms: None,
            speed_permille: None,
            color: vec![
                SceneColorKey {
                    time_ms: 0,
                    value: "#000000".into(),
                },
                SceneColorKey {
                    time_ms: 100,
                    value: "#FFFFFF".into(),
                },
            ],
            rotate: Vec::new(),
            easing: None,
            scale: Vec::new(),
            offset: Vec::new(),
            frame: Vec::new(),
        }),
        rotate: None,
        blend: None,
        align: None,
        z: None,
        wrap: None,
        mode: None,
        focus: None,
        children: Vec::new(),
    });
    assert!(device.render_scene());
    assert_eq!(device.pixel_rgb565(0, 0), Some(0), "black at t=0");
    device.clock.tick = 50;
    assert!(device.render_scene());
    let mid = device.pixel_rgb565(0, 0).unwrap();
    assert!(mid != 0 && mid != 0xFFFF, "grey midway: {mid:#06x}");
    device.clock.tick = 100;
    assert!(device.render_scene());
    assert_eq!(device.pixel_rgb565(0, 0), Some(0xFFFF), "white at the end");
}

#[test]
fn scene_rotate_animation_swaps_axes_and_hit_test() {
    // N827/N828: a rotate track drives both rasterisation and hit-testing.
    use mdc_protocol::{SceneAnim, SceneNode};
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 8,
        height: 8,
        ..BoardProfile::default()
    });
    device.scene = Some(SceneNode {
        name: "wide".into(),
        kind: "rect".into(),
        rect: Some([0, 0, 8, 2]),
        value: Some("#FFFFFF".into()),
        value2: None,
        opacity: None,
        visible: None,
        anim: Some(SceneAnim {
            duration_ms: 100,
            looping: false,
            opacity: Vec::new(),
            rect: Vec::new(),
            focus: Vec::new(),
            delay_ms: None,
            speed_permille: None,
            color: Vec::new(),
            rotate: vec![[0, 0], [100, 90]],
            easing: None,
            scale: Vec::new(),
            offset: Vec::new(),
            frame: Vec::new(),
        }),
        rotate: None,
        blend: None,
        align: None,
        z: None,
        wrap: None,
        mode: None,
        focus: None,
        children: Vec::new(),
    });
    assert!(device.render_scene());
    assert_eq!(device.pixel_rgb565(7, 1), Some(0xFFFF), "wide at t=0");
    assert_eq!(device.hit_test(7, 1).as_deref(), Some("wide"));
    device.clock.tick = 100;
    assert!(device.render_scene());
    assert_eq!(device.pixel_rgb565(1, 7), Some(0xFFFF), "tall at the end");
    assert_eq!(device.pixel_rgb565(7, 1), Some(0), "not wide at the end");
    assert_eq!(device.hit_test(1, 7).as_deref(), Some("wide"));
    assert!(
        device.hit_test(7, 1).is_none(),
        "hit-test follows the rotate"
    );
}

#[test]
fn scene_scale_and_offset_animation_reach_the_device() {
    // N846/N847: scale and offset tracks change rasterisation and the hit
    // test together, on the device clock.
    use mdc_protocol::{SceneAnim, SceneNode, SceneOffsetKey};
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 16,
        height: 16,
        ..BoardProfile::default()
    });
    device.scene = Some(SceneNode {
        name: "box".into(),
        kind: "rect".into(),
        rect: Some([4, 4, 4, 4]),
        value: Some("#FFFFFF".into()),
        value2: None,
        opacity: None,
        visible: None,
        anim: Some(SceneAnim {
            duration_ms: 100,
            looping: false,
            opacity: Vec::new(),
            rect: Vec::new(),
            focus: Vec::new(),
            delay_ms: None,
            speed_permille: None,
            color: Vec::new(),
            rotate: Vec::new(),
            easing: None,
            scale: vec![[0, 100], [100, 200]],
            offset: vec![
                SceneOffsetKey {
                    time_ms: 0,
                    dx: 0,
                    dy: 0,
                },
                SceneOffsetKey {
                    time_ms: 100,
                    dx: 4,
                    dy: -2,
                },
            ],
            frame: Vec::new(),
        }),
        rotate: None,
        blend: None,
        align: None,
        z: None,
        wrap: None,
        mode: None,
        focus: None,
        children: Vec::new(),
    });
    assert!(device.render_scene());
    assert_eq!(device.pixel_rgb565(4, 4), Some(0xFFFF), "authored box");
    assert_eq!(device.pixel_rgb565(2, 2), Some(0), "not scaled yet");
    assert_eq!(device.hit_test(5, 5).as_deref(), Some("box"));

    device.clock.tick = 100;
    assert!(device.render_scene());
    // 200% about the centre gives [2,2,8,8], then +4/-2 gives [6,0,8,8].
    assert_eq!(device.pixel_rgb565(6, 0), Some(0xFFFF), "scaled and moved");
    assert_eq!(device.pixel_rgb565(10, 5), Some(0xFFFF));
    assert_eq!(device.pixel_rgb565(5, 5), Some(0), "left of the moved box");
    assert_eq!(device.hit_test(10, 5).as_deref(), Some("box"));
    assert!(device.hit_test(5, 5).is_none());
}

#[test]
fn sprite_frame_animation_swaps_the_rendered_atlas_frame() {
    // N855/N856: a sprite frame track changes the atlas index on the device
    // clock without touching any other payload.
    use mdc_protocol::{SceneAnim, SceneNode};
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 8,
        height: 8,
        ..BoardProfile::default()
    });
    let mut rgb565 = vec![0u8; 8 * 4 * 2];
    for y in 0..4u32 {
        for x in 0..8u32 {
            let value: u16 = if x < 4 { 0xF800 } else { 0x07E0 };
            let offset = ((y * 8 + x) * 2) as usize;
            rgb565[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
    }
    device.atlas = Some(mdc_model::AtlasBlob {
        sprite_width: 4,
        sprite_height: 4,
        frames: 2,
        columns: 2,
        rows: 1,
        rgb565,
        clips: Vec::new(),
        mips: Vec::new(),
    });
    device.scene = Some(SceneNode {
        name: "face".into(),
        kind: "sprite".into(),
        rect: Some([0, 0, 4, 4]),
        value: Some("0".into()),
        value2: None,
        opacity: None,
        visible: None,
        anim: Some(SceneAnim {
            duration_ms: 100,
            frame: vec![[0, 1], [50, 0]],
            ..Default::default()
        }),
        rotate: None,
        blend: None,
        align: None,
        z: None,
        wrap: None,
        mode: None,
        focus: None,
        children: Vec::new(),
    });
    assert!(device.render_scene());
    assert_eq!(device.pixel_rgb565(1, 1), Some(0x07E0), "frame 1");
    device.clock.tick = 50;
    assert!(device.render_scene());
    assert_eq!(device.pixel_rgb565(1, 1), Some(0xF800), "frame 0");
    assert_eq!(device.hit_test(2, 2).as_deref(), Some("face"));
}

#[test]
fn point_geometry_hit_tests_ignore_empty_bounding_box_corners() {
    // N857: polygon and polyline hits use the actual points, not the rect.
    use mdc_protocol::SceneNode;
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 8,
        height: 8,
        ..BoardProfile::default()
    });
    let node = |kind: &str, value2: &str| SceneNode {
        name: kind.into(),
        kind: kind.into(),
        rect: Some([0, 0, 8, 8]),
        value: Some("#FFFFFF".into()),
        value2: Some(value2.into()),
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
    };
    device.scene = Some(node("polygon", "0,0;8,0;0,8"));
    assert!(device.render_scene());
    assert_eq!(device.hit_test(1, 1).as_deref(), Some("polygon"));
    assert!(device.hit_test(6, 6).is_none(), "outside the triangle");
    assert!(
        device.hit_test(7, 1).is_none(),
        "inside the rect but outside the triangle"
    );

    device.scene = Some(node("polyline", "1,4;7,4"));
    assert!(device.render_scene());
    assert_eq!(device.hit_test(4, 4).as_deref(), Some("polyline"));
    assert!(
        device.hit_test(4, 7).is_none(),
        "the bounding box is not a hit"
    );
}

#[test]
fn path_hit_tests_follow_the_stroke_and_scaled_points() {
    // N854/N858: a path hit follows its flattened stroke, and the point
    // geometry is transformed by the scale/offset tracks before the test.
    use mdc_protocol::{SceneAnim, SceneNode};
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 16,
        height: 16,
        ..BoardProfile::default()
    });
    device.scene = Some(SceneNode {
        name: "stroke".into(),
        kind: "path".into(),
        rect: Some([0, 0, 16, 16]),
        value: Some("#FFFFFF".into()),
        value2: Some("M0,0;L8,0;Q12,0 12,8".into()),
        opacity: None,
        visible: None,
        anim: Some(SceneAnim {
            duration_ms: 100,
            scale: vec![[0, 100], [100, 200]],
            offset: vec![mdc_protocol::SceneOffsetKey {
                time_ms: 0,
                dx: 4,
                dy: 4,
            }],
            ..Default::default()
        }),
        rotate: None,
        blend: None,
        align: None,
        z: None,
        wrap: None,
        mode: None,
        focus: None,
        children: Vec::new(),
    });
    assert!(device.render_scene());
    assert_eq!(device.hit_test(12, 4).as_deref(), Some("stroke"));
    assert!(device.hit_test(4, 12).is_none(), "away from the stroke");

    device.clock.tick = 100;
    assert!(device.render_scene());
    // 200% about (8,8) plus +4/+4 moves the curve; its midpoint is (18,0).
    // The old horizontal segment at y=4 no longer hits.
    assert!(device.hit_test(12, 4).is_none());
    assert_eq!(device.hit_test(18, 0).as_deref(), Some("stroke"));
}

#[test]
fn hidden_scene_nodes_are_skipped() {
    use mdc_protocol::SceneNode;
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 8,
        height: 8,
        ..BoardProfile::default()
    });
    device.scene = Some(SceneNode {
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
            SceneNode {
                name: "hidden".into(),
                kind: "rect".into(),
                rect: Some([0, 0, 8, 8]),
                value: Some("#FFFFFF".into()),
                value2: None,
                opacity: None,
                visible: Some(false),
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
            SceneNode {
                name: "shown".into(),
                kind: "rect".into(),
                rect: Some([0, 0, 4, 4]),
                value: Some("#00FF00".into()),
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
                children: Vec::new(),
            },
        ],
    });
    assert!(device.render_scene());
    // The hidden full-surface rect is skipped; the shown 4x4 rect is drawn.
    assert_eq!(device.pixel_rgb565(0, 0), Some(0x07E0), "shown rect drawn");
    assert_eq!(device.pixel_rgb565(7, 7), Some(0), "hidden rect skipped");
    assert_eq!(device.hit_test(7, 7), None, "hidden node not hit");
    assert_eq!(device.hit_test(1, 1).as_deref(), Some("shown"));
}

#[test]
fn path_arc_and_smooth_quad_render() {
    // N753: `A` flattens an elliptical arc and `T` reflects the previous
    // quadratic control; both reach the framebuffer.
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 24,
        height: 16,
        ..BoardProfile::default()
    });
    device.scene = Some(
        serde_json::from_str(
            r##"{"name":"root","kind":"path",
                    "value2":"M2,12;A6,6,0,0,1,14,12;T22,12",
                    "value":"#FFFFFF"}"##,
        )
        .unwrap(),
    );
    assert!(device.render_scene());
    // The upper half of the arc passes through the top of the box.
    assert_ne!(device.pixel_rgb565(8, 6), Some(0), "arc top drawn");
    assert_ne!(device.pixel_rgb565(2, 12), Some(0), "arc start drawn");
    assert_ne!(device.pixel_rgb565(22, 12), Some(0), "smooth end drawn");
    assert_eq!(
        device.pixel_rgb565(8, 15),
        Some(0),
        "lower half is not part of the arc"
    );
}

#[test]
fn relative_path_commands_and_smooth_cubic_render() {
    // N763: a relative command stream draws the same geometry as absolute
    // coordinates, and `S` bends away from the straight chord.
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 24,
        height: 24,
        ..BoardProfile::default()
    });
    let render = |device: &mut FakeDevice, value2: &str| {
        device.scene = Some(
            serde_json::from_str(&format!(
                r##"{{"name":"p","kind":"path","value2":"{value2}","value":"#FFFFFF"}}"##
            ))
            .unwrap(),
        );
        assert!(device.render_scene());
    };
    render(&mut device, "M2,20;l20,0");
    assert_ne!(device.pixel_rgb565(12, 20), Some(0), "relative line drawn");
    assert_eq!(device.pixel_rgb565(12, 2), Some(0));
    render(&mut device, "M2,20;c4,-12 12,-12 16,0;s4,4 5,0");
    assert_ne!(device.pixel_rgb565(2, 20), Some(0), "cubic starts");
    assert_ne!(device.pixel_rgb565(23, 20), Some(0), "smooth cubic ends");
    assert_eq!(
        device.pixel_rgb565(12, 4),
        Some(0),
        "the curve stays above the top edge"
    );
    // N773: horizontal/vertical commands.
    render(&mut device, "M2,2;H20;v10;h-8;V2");
    assert_ne!(device.pixel_rgb565(10, 2), Some(0), "horizontal");
    assert_ne!(device.pixel_rgb565(20, 7), Some(0), "vertical");
    assert_ne!(device.pixel_rgb565(12, 12), Some(0), "relative horizontal");
    // N783: one `L` with two coordinate pairs draws both segments.
    render(&mut device, "M2,4;L18,4 18,16");
    assert_ne!(device.pixel_rgb565(10, 4), Some(0), "repeated line");
    assert_ne!(device.pixel_rgb565(18, 10), Some(0), "second implicit line");
    assert_eq!(
        device.pixel_rgb565(4, 10),
        Some(0),
        "the corner is not filled"
    );
    // N793: width=1 keeps the orthogonal neighbour clear; width=3 paints it.
    let render_mode = |device: &mut FakeDevice, mode: &str| {
        device.scene = Some(
                serde_json::from_str(&format!(
                    r##"{{"name":"p","kind":"path","mode":"{mode}","value2":"M4,8;L16,8","value":"#FFFFFF"}}"##
                ))
                .unwrap(),
            );
        assert!(device.render_scene());
    };
    render_mode(&mut device, "width=1");
    assert_ne!(device.pixel_rgb565(4, 8), Some(0), "endpoint on the stroke");
    assert_ne!(device.pixel_rgb565(16, 8), Some(0), "far endpoint");
    assert_eq!(
        device.pixel_rgb565(4, 9),
        Some(0),
        "width 1 stays one pixel"
    );
    render_mode(&mut device, "width=3");
    assert_ne!(device.pixel_rgb565(4, 8), Some(0), "center stays on");
    assert_ne!(
        device.pixel_rgb565(4, 9),
        Some(0),
        "width 3 covers the neighbour"
    );
    assert_ne!(
        device.pixel_rgb565(4, 7),
        Some(0),
        "width 3 covers both sides"
    );
    assert_eq!(
        device.pixel_rgb565(4, 10),
        Some(0),
        "width 3 stops at one pixel"
    );
    render_mode(&mut device, "width=nope");
    assert_eq!(
        device.pixel_rgb565(4, 9),
        Some(0),
        "invalid width stays 1 pixel"
    );
    render_mode(&mut device, "width=0");
    assert_eq!(
        device.pixel_rgb565(4, 9),
        Some(0),
        "out-of-range width stays 1"
    );
    // N803: dash=4 width=3 paints a thick on-segment. The stamp must not
    // cover the first off-phase sample (8,8); a later gap pixel stays clear
    // too. The neighbour of an on-segment pixel (5,9) stays covered.
    render_mode(&mut device, "dash=4,width=3");
    assert_ne!(device.pixel_rgb565(5, 8), Some(0), "dash segment center");
    assert_ne!(
        device.pixel_rgb565(5, 9),
        Some(0),
        "width covers the dash segment"
    );
    assert_eq!(
        device.pixel_rgb565(8, 8),
        Some(0),
        "the first gap sample stays clear"
    );
    assert_eq!(device.pixel_rgb565(9, 8), Some(0), "dash gap stays clear");
    assert_eq!(
        device.pixel_rgb565(9, 9),
        Some(0),
        "width does not fill the gap"
    );
    render_mode(&mut device, "dash=4,width=nope");
    assert_ne!(device.pixel_rgb565(4, 8), Some(0), "dashed stroke still on");
    assert_eq!(
        device.pixel_rgb565(4, 9),
        Some(0),
        "invalid width stays one pixel on a dash"
    );
    assert_eq!(device.pixel_rgb565(8, 8), Some(0), "the gap stays clear");
    render_mode(&mut device, "width=1");
    assert_ne!(
        device.pixel_rgb565(8, 8),
        Some(0),
        "a solid path keeps the gap pixel"
    );
    // N813: `phase` shifts the dash period. With dash 4, phase 4 starts in
    // the gap; phase 6 starts mid-gap and the on-segment begins at x=6.
    render_mode(&mut device, "dash=4,phase=4");
    assert_eq!(
        device.pixel_rgb565(4, 8),
        Some(0),
        "phase 4 starts on a gap"
    );
    assert_ne!(device.pixel_rgb565(8, 8), Some(0), "the next period is on");
    render_mode(&mut device, "dash=4,phase=6");
    assert_eq!(
        device.pixel_rgb565(4, 8),
        Some(0),
        "mid-gap start stays clear"
    );
    assert_eq!(device.pixel_rgb565(5, 8), Some(0), "gap still clear");
    assert_ne!(
        device.pixel_rgb565(6, 8),
        Some(0),
        "on-segment after the mid-gap phase"
    );
    // The width stamp follows the shifted segment but never covers a gap
    // sample centre.
    render_mode(&mut device, "dash=4,phase=6,width=3");
    assert_eq!(
        device.pixel_rgb565(5, 8),
        Some(0),
        "width leaves the gap sample clear"
    );
    assert_ne!(
        device.pixel_rgb565(6, 9),
        Some(0),
        "the shifted on-segment is stamped thick"
    );
    // An invalid phase keeps the unshifted dash.
    render_mode(&mut device, "dash=4,phase=nope");
    assert_ne!(device.pixel_rgb565(4, 8), Some(0), "dash starts on");
}

#[test]
fn sheared_image_and_sprite_sample_the_parallelogram() {
    // N788: a group shear of x' = x + y. Pixels in the bounding-box corner
    // stay clear; the left/right of the image stay on the parallelogram.
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 20,
        height: 10,
        ..BoardProfile::default()
    });
    device.scene = Some(
        serde_json::from_str(
            r##"{"name":"g","kind":"group","value2":"1000,0","children":[
                    {"name":"img","kind":"image","rect":[0,0,8,4],
                     "value":"F80007E0","value2":"2,1"}]}"##,
        )
        .unwrap(),
    );
    assert!(device.render_scene());
    let red = |pixel: u16| (pixel >> 11) > ((pixel >> 5) & 0x3f);
    let green = |pixel: u16| ((pixel >> 5) & 0x3f) > (pixel >> 11);
    assert_eq!(device.pixel_rgb565(0, 3), Some(0), "outside the shear");
    assert!(
        red(device.pixel_rgb565(3, 2).unwrap()),
        "left texel {:?}",
        device.pixel_rgb565(3, 2)
    );
    assert!(
        green(device.pixel_rgb565(9, 2).unwrap()),
        "right texel {:?}",
        device.pixel_rgb565(9, 2)
    );

    device.atlas = Some(mdc_model::AtlasBlob {
        sprite_width: 2,
        sprite_height: 1,
        frames: 1,
        columns: 1,
        rows: 1,
        rgb565: vec![0x00, 0xF8, 0xE0, 0x07],
        clips: Vec::new(),
        mips: Vec::new(),
    });
    device.scene = Some(
        serde_json::from_str(
            r##"{"name":"g","kind":"group","value2":"1000,0","children":[
                    {"name":"sp","kind":"sprite","rect":[0,0,8,4],"value":"0"}]}"##,
        )
        .unwrap(),
    );
    assert!(device.render_scene());
    assert_eq!(
        device.pixel_rgb565(0, 3),
        Some(0),
        "sprite outside the shear"
    );
    assert!(red(device.pixel_rgb565(3, 2).unwrap()), "sprite left");
    assert!(green(device.pixel_rgb565(9, 2).unwrap()), "sprite right");

    // N798: flip and a half-turn swap the parallelogram's ends; both together
    // restore the original ends. The empty corner stays clear.
    let sheared = |mode: &str, rotate: u16| {
        serde_json::from_str::<mdc_protocol::SceneNode>(&format!(
            r##"{{"name":"g","kind":"group","value2":"1000,0","children":[
                    {{"name":"img","kind":"image","rect":[0,0,8,4],
                     "mode":"{mode}","rotate":{rotate},
                     "value":"F80007E0","value2":"2,1"}}]}}"##
        ))
        .unwrap()
    };
    let ends = |device: &mut FakeDevice, mode: &str, rotate: u16| {
        device.scene = Some(sheared(mode, rotate));
        assert!(device.render_scene());
        assert_eq!(device.pixel_rgb565(0, 3), Some(0), "corner stays outside");
        (
            device.pixel_rgb565(3, 2).unwrap(),
            device.pixel_rgb565(9, 2).unwrap(),
        )
    };
    let (left, right) = ends(&mut device, "flip-h", 0);
    assert!(green(left), "flip-h swaps the left end: {left:#06x}");
    assert!(red(right), "flip-h swaps the right end: {right:#06x}");
    let (left, right) = ends(&mut device, "", 180);
    assert!(green(left), "rotate 180 swaps the left end: {left:#06x}");
    assert!(red(right), "rotate 180 swaps the right end: {right:#06x}");
    let (left, right) = ends(&mut device, "flip-h", 180);
    assert!(
        red(left),
        "flip-h plus 180 restores the left end: {left:#06x}"
    );
    assert!(
        green(right),
        "flip-h plus 180 restores the right end: {right:#06x}"
    );
}

#[test]
fn alpha_atlas_mip_selection_is_visible_to_the_device() {
    // N789: the device crate uses the same coarse colour level as the host,
    // with the level-0 alpha mask resampled onto it.
    let base = mdc_model::TextureBlob {
        width: 8,
        height: 8,
        rgb565: [0xF8u8, 0x00].repeat(64),
        alpha: Vec::new(),
        palette: Vec::new(),
        indices: Vec::new(),
        mips: Vec::new(),
    };
    let mut mipped = mdc_bake::mip_texture_blob(&base, 8, false);
    let mut alpha = vec![0u8; 8];
    for y in 0..8u32 {
        for x in 0..4u32 {
            let index = (y * 8 + x) as usize;
            alpha[index / 8] |= 1 << (index % 8);
        }
    }
    mipped.alpha = alpha;
    let selected = mdc_bake::texture_for_target(&mipped, (2, 2));
    assert_eq!((selected.width, selected.height), (2, 2));
    assert!(selected.alpha_at(0.25, 0.25));
    assert!(!selected.alpha_at(0.75, 0.25));
}

#[test]
fn mask_invert_and_feather_soften_the_edge() {
    // N754: `invert` keeps the outside of the polygon; `feather=N` fades the
    // boundary instead of cutting it.
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 16,
        height: 8,
        ..BoardProfile::default()
    });
    let scene = |mode: &str| {
        serde_json::from_str::<mdc_protocol::SceneNode>(&format!(
            r##"{{"name":"root","kind":"mask","value2":"0,0;8,0;8,8;0,8",
                     "mode":"{mode}","children":[
                       {{"name":"fill","kind":"rect","rect":[0,0,16,8],"value":"#FF0000"}}]}}"##
        ))
        .unwrap()
    };
    device.scene = Some(scene("invert"));
    assert!(device.render_scene());
    assert_eq!(device.pixel_rgb565(2, 4), Some(0), "left half masked out");
    assert_eq!(device.pixel_rgb565(12, 4), Some(0xF800), "right half drawn");

    device.scene = Some(scene("feather=3"));
    assert!(device.render_scene());
    assert_eq!(
        device.pixel_rgb565(2, 4),
        Some(0xF800),
        "inside stays opaque"
    );
    let edge = device.pixel_rgb565(9, 4).unwrap();
    assert!(
        edge != 0 && edge != 0xF800,
        "the feathered edge blends toward the background: {edge:#06x}"
    );
    assert_eq!(device.pixel_rgb565(15, 4), Some(0), "far outside is hidden");
}

#[test]
fn group_skew_shears_children() {
    // N759: a group's `value2` shear offsets x by ky·y (permille), so a
    // vertical rect leans right and hit-testing follows the shear.
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 16,
        height: 16,
        ..BoardProfile::default()
    });
    device.scene = Some(
        serde_json::from_str(
            r##"{"name":"root","kind":"group","value2":"1000,0","children":[
                     {"name":"bar","kind":"rect","rect":[0,0,4,12],"value":"#FFFFFF"}]}"##,
        )
        .unwrap(),
    );
    assert!(device.render_scene());
    assert_ne!(device.pixel_rgb565(2, 1), Some(0), "top is at x=0");
    assert_ne!(device.pixel_rgb565(12, 10), Some(0), "bottom shifted right");
    assert_eq!(
        device.pixel_rgb565(1, 10),
        Some(0),
        "left of the lean is empty"
    );
    assert_eq!(device.hit_test(12, 10).as_deref(), Some("bar"));
    // N768: (3,10) is inside the axis-aligned box but outside the lean.
    assert_eq!(device.hit_test(3, 10), None, "exact inverse hit test");
    // A malformed shear leaves the transform unchanged.
    device.scene = Some(
        serde_json::from_str(
            r##"{"name":"root","kind":"group","value2":"nope","children":[
                     {"name":"bar","kind":"rect","rect":[0,0,4,12],"value":"#FFFFFF"}]}"##,
        )
        .unwrap(),
    );
    assert!(device.render_scene());
    assert_ne!(device.pixel_rgb565(1, 10), Some(0), "unskewed");
}

#[test]
fn skewed_clip_and_mask_follow_the_shear() {
    // N778: a clip inside a sheared group composites its parallelogram, and
    // a mask polygon is sheared exactly before rasterizing.
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 20,
        height: 16,
        ..BoardProfile::default()
    });
    let show = |device: &mut FakeDevice, scene: &str| {
        device.scene = Some(serde_json::from_str(scene).unwrap());
        assert!(device.render_scene());
    };
    show(
        &mut device,
        r##"{"name":"g","kind":"group","value2":"1000,0","children":[
                 {"name":"c","kind":"clip","rect":[0,0,4,12],"children":[
                   {"name":"fill","kind":"rect","rect":[0,0,20,16],"value":"#FF0000"}]}]}"##,
    );
    assert_ne!(device.pixel_rgb565(12, 10), Some(0), "inside the lean");
    assert_eq!(device.pixel_rgb565(4, 10), Some(0), "left of the lean");
    assert_eq!(device.pixel_rgb565(18, 1), Some(0), "right of the lean");

    show(
        &mut device,
        r##"{"name":"g","kind":"group","value2":"1000,0","children":[
                 {"name":"m","kind":"mask","value2":"0,0;4,0;0,12","children":[
                   {"name":"fill","kind":"rect","rect":[0,0,20,16],"value":"#00FF00"}]}]}"##,
    );
    assert_ne!(device.pixel_rgb565(3, 2), Some(0), "mask point sheared");
    assert_eq!(
        device.pixel_rgb565(10, 2),
        Some(0),
        "outside the sheared mask"
    );

    // N808: an image child is masked to the same parallelogram. Pixels the
    // image would paint outside the lean stay clear.
    show(
        &mut device,
        r##"{"name":"g","kind":"group","value2":"1000,0","children":[
                 {"name":"c","kind":"clip","rect":[0,0,4,12],"children":[
                   {"name":"img","kind":"image","rect":[0,0,16,16],
                    "value":"F800F800F800F800","value2":"2,2"}]}]}"##,
    );
    assert_eq!(
        device.pixel_rgb565(12, 10),
        Some(0xF800),
        "image pixel inside the parallelogram"
    );
    assert_eq!(
        device.pixel_rgb565(16, 10),
        Some(0),
        "image pixel outside the parallelogram"
    );
    assert_eq!(
        device.pixel_rgb565(4, 10),
        Some(0),
        "axis-aligned box outside the lean"
    );
    // N818: a sprite child of the sheared clip is masked the same way; an
    // axis-aligned clip still composites the sprite's own rect.
    device.atlas = Some(mdc_model::AtlasBlob {
        sprite_width: 1,
        sprite_height: 1,
        frames: 1,
        columns: 1,
        rows: 1,
        rgb565: vec![0x00, 0xF8],
        clips: Vec::new(),
        mips: Vec::new(),
    });
    show(
        &mut device,
        r##"{"name":"g","kind":"group","value2":"1000,0","children":[
                 {"name":"c","kind":"clip","rect":[0,0,4,12],"children":[
                   {"name":"sp","kind":"sprite","rect":[0,0,16,16],"value":"0"}]}]}"##,
    );
    assert_eq!(
        device.pixel_rgb565(12, 10),
        Some(0xF800),
        "sprite pixel inside the parallelogram"
    );
    assert_eq!(
        device.pixel_rgb565(16, 10),
        Some(0),
        "sprite pixel outside the parallelogram"
    );
    assert_eq!(
        device.pixel_rgb565(4, 10),
        Some(0),
        "sprite outside the lean stays clear"
    );
    show(
        &mut device,
        r##"{"name":"c","kind":"clip","rect":[0,0,8,8],"children":[
                 {"name":"sp","kind":"sprite","rect":[0,0,8,8],"value":"0"}]}"##,
    );
    assert_eq!(
        device.pixel_rgb565(4, 4),
        Some(0xF800),
        "axis-aligned sprite is unchanged"
    );
    assert_eq!(
        device.pixel_rgb565(9, 4),
        Some(0),
        "axis-aligned clip bounds the sprite"
    );
}

#[test]
fn mirror_refuses_a_non_primary_surface() {
    // N457: only `main` is mirrored; `overlay` is refused explicitly.
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        overlay_width: 4,
        overlay_height: 2,
        mirror: true,
        ..BoardProfile::default()
    });
    let command = |surface: &str| mdc_protocol::MirrorCommand {
        action: "start".into(),
        surface_id: surface.into(),
        mode: "full-frame".into(),
        max_fps: 15,
        format: "rgb565".into(),
    };
    let packet = |surface: &str| Packet {
        version: mdc_protocol::VERSION,
        kind: MessageType::MirrorCtrl,
        flags: 0,
        request_id: 1,
        payload: mdc_protocol::encode_control(&command(surface)).unwrap(),
    };
    assert!(device.handle_mirror_ctrl(packet("overlay")).is_ok());
    assert!(!device.mirror_active, "overlay is refused");
    assert!(device.handle_mirror_ctrl(packet("main")).is_ok());
    assert!(device.mirror_active, "main is mirrored");
}

#[test]
fn hit_test_finds_the_topmost_node() {
    use mdc_protocol::SceneNode;
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 16,
        height: 16,
        ..BoardProfile::default()
    });
    let rect = |name: &str, r: [u16; 4]| SceneNode {
        name: name.into(),
        kind: "rect".into(),
        rect: Some(r),
        value: Some("#FFFFFF".into()),
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
        children: Vec::new(),
    };
    device.scene = Some(SceneNode {
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
        children: vec![rect("back", [0, 0, 8, 8]), rect("front", [2, 2, 4, 4])],
    });
    // The overlapping topmost child wins.
    assert_eq!(device.hit_test(3, 3).as_deref(), Some("front"));
    // The lower child is hit outside the front rect.
    assert_eq!(device.hit_test(7, 7).as_deref(), Some("back"));
    // Outside every rect.
    assert_eq!(device.hit_test(12, 12), None);
}

#[test]
fn scene_clip_bounds_children() {
    use mdc_protocol::SceneNode;
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 8,
        height: 8,
        ..BoardProfile::default()
    });
    let fill = |rect: [u16; 4]| SceneNode {
        name: "f".into(),
        kind: "rect".into(),
        rect: Some(rect),
        value: Some("#FFFFFF".into()),
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
        children: Vec::new(),
    };
    device.scene = Some(SceneNode {
        name: "c".into(),
        kind: "clip".into(),
        rect: Some([2, 2, 4, 4]),
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
        // The child fills 8x8 relative to the clip origin, so it would cover
        // everything without the clip rect.
        children: vec![fill([0, 0, 8, 8])],
    });
    assert!(device.render_scene());
    assert_eq!(device.pixel_rgb565(3, 3), Some(0xFFFF), "inside clip");
    assert_eq!(device.pixel_rgb565(0, 0), Some(0), "outside clip top-left");
    assert_eq!(
        device.pixel_rgb565(7, 7),
        Some(0),
        "outside clip bottom-right"
    );
    assert_eq!(
        device.pixel_rgb565(5, 5),
        Some(0xFFFF),
        "clip edge included"
    );
    assert_eq!(device.pixel_rgb565(6, 6), Some(0), "just past clip");
}

#[test]
fn scene_rounded_rect_clips_the_corners() {
    use mdc_protocol::SceneNode;
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 8,
        height: 8,
        ..BoardProfile::default()
    });
    device.scene = Some(SceneNode {
        name: "r".into(),
        kind: "rounded-rect".into(),
        rect: Some([0, 0, 8, 8]),
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
    });
    assert!(device.render_scene());
    assert_eq!(device.pixel_rgb565(0, 0), Some(0), "corner clipped");
    assert_ne!(device.pixel_rgb565(0, 4), Some(0), "edge middle filled");
    assert_ne!(device.pixel_rgb565(4, 4), Some(0), "center filled");
}

#[test]
fn scene_draws_polygons_and_polylines() {
    // N723: a filled triangle and a stroked polyline rasterize at their
    // absolute points; pixels outside the triangle stay clear.
    use mdc_protocol::SceneNode;
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 8,
        height: 8,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    let node = |name: &str, kind: &str, value: &str, points: &str| SceneNode {
        name: name.into(),
        kind: kind.into(),
        rect: Some([0, 0, 8, 8]),
        children: Vec::new(),
        value: Some(value.into()),
        value2: Some(points.into()),
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
    };
    let root = SceneNode {
        name: "root".into(),
        kind: "group".into(),
        rect: None,
        children: vec![
            node("tri", "polygon", "#FF0000", "1,1;6,1;1,6"),
            node("score", "polyline", "#00FF00", "0,7;7,7"),
        ],
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
    };
    session.set_scene(root).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_scene());
    assert_eq!(device.pixel_rgb565(2, 2), Some(0xF800));
    assert_eq!(device.pixel_rgb565(5, 5), Some(0));
    assert_eq!(device.pixel_rgb565(3, 7), Some(0x07E0));
    assert_eq!(device.pixel_rgb565(3, 6), Some(0));
    // A 65-point polygon is over the protocol bound and is not parsed.
    let many = (0..65)
        .map(|i| format!("{},{}", i % 8, i / 8))
        .collect::<Vec<_>>()
        .join(";");
    let too_many = node("many", "polygon", "#FFFFFF", &many);
    assert!(too_many.points().is_none());
}

#[test]
fn scene_draws_sectors_arcs_and_bezier_paths() {
    // N733/N734: pie fills an angle range, arc strokes the ellipse, and a
    // quadratic path bends away from the straight chord.
    use mdc_protocol::SceneNode;
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 24,
        height: 24,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    let node = |name: &str, kind: &str, rect: [u16; 4], value: &str, value2: &str| SceneNode {
        name: name.into(),
        kind: kind.into(),
        rect: Some(rect),
        children: Vec::new(),
        value: Some(value.into()),
        value2: Some(value2.into()),
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
    };
    let root = SceneNode {
        name: "root".into(),
        kind: "group".into(),
        rect: None,
        children: vec![
            node("sector", "pie", [0, 0, 12, 12], "#FF0000", "0,90"),
            node("ring", "arc", [12, 1, 12, 14], "#00FF00", "0,360"),
            node(
                "curve",
                "path",
                [0, 0, 24, 24],
                "#0000FF",
                "2,22;12,10;22,22",
            ),
        ],
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
    };
    session.set_scene(root).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_scene());
    assert_eq!(device.pixel_rgb565(9, 9), Some(0xF800), "0..90 sector");
    assert_eq!(device.pixel_rgb565(2, 9), Some(0), "outside the sector");
    assert_eq!(device.pixel_rgb565(9, 2), Some(0), "outside the sector");
    assert_eq!(device.pixel_rgb565(18, 1), Some(0x07E0), "arc top");
    assert_eq!(device.pixel_rgb565(18, 8), Some(0), "arc interior");
    assert_eq!(device.pixel_rgb565(12, 16), Some(0x001F), "curve apex");
    assert_eq!(device.pixel_rgb565(12, 21), Some(0), "chord midpoint");
}

#[test]
fn mesh_clip_cross_fade_blends_and_expires() {
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 4,
        height: 4,
        ..BoardProfile::default()
    });
    device.begin_mesh_crossfade();
    assert_eq!(device.mesh_crossfade_ms, MESH_CROSSFADE_MS);
    assert_eq!(device.mesh_prev_pixels.len(), device.pixels.len());

    // Full weight copies; zero keeps.
    let src = [0xE0u8, 0x07, 0x00, 0xF8];
    let mut dst = vec![0x00u8, 0xF8, 0xE0, 0x07];
    mix_rgb565(&mut dst, &src, 4);
    assert_eq!(dst, src);
    let mut dst = vec![0x00u8, 0xF8, 0xE0, 0x07];
    mix_rgb565(&mut dst, &src, 0);
    assert_eq!(dst, vec![0x00, 0xF8, 0xE0, 0x07]);

    // Half weight blends red toward green (r channel strictly between).
    let mut dst = vec![0x00u8, 0xF8, 0x00, 0xF8];
    mix_rgb565(&mut dst, &[0xE0u8, 0x07, 0xE0, 0x07], 2);
    let red = (u16::from_le_bytes([dst[0], dst[1]]) >> 11) & 0x1f;
    assert!(red > 0 && red < 0x1f, "red channel blended: {red}");
}

#[test]
fn scene_draws_circles_and_lines() {
    use mdc_protocol::SceneNode;
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 16,
        height: 16,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    let node = |name: &str, kind: &str, rect: [u16; 4], value: &str| SceneNode {
        name: name.into(),
        kind: kind.into(),
        rect: Some(rect),
        children: Vec::new(),
        value: Some(value.into()),
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
    let scene = SceneNode {
        name: "root".into(),
        kind: "group".into(),
        rect: None,
        value: None,
        children: vec![
            node("disc", "circle", [0, 0, 8, 8], "#FF0000"),
            node("diag", "line", [0, 15, 15, 0], "#00FF00"),
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
    assert_eq!(device.pixel_rgb565(4, 4), Some(0xF800), "circle centre");
    assert_eq!(
        device.pixel_rgb565(0, 0),
        Some(0),
        "ellipse corner is clear"
    );
    assert_eq!(device.pixel_rgb565(8, 7), Some(0x07E0), "line pixel");
}

#[test]
fn scene_group_offsets_and_scales_children() {
    use mdc_protocol::SceneNode;
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 16,
        height: 16,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

    let rect = |name: &str, geometry: [u16; 4], color: &str| SceneNode {
        name: name.into(),
        kind: "rect".into(),
        rect: Some(geometry),
        children: Vec::new(),
        value: Some(color.into()),
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
    // Group offset (4, 2) and 2× scale: [0,0,2,2] red -> [4,2,4,4].
    let scene = SceneNode {
        name: "root".into(),
        kind: "group".into(),
        rect: Some([4, 2, 2000, 2000]),
        children: vec![rect("bg", [0, 0, 2, 2], "#FF0000")],
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
    assert_eq!(device.pixel_rgb565(4, 2), Some(0xF800), "offset applied");
    assert_eq!(device.pixel_rgb565(7, 5), Some(0xF800), "2x scale applied");
    assert_eq!(device.pixel_rgb565(0, 0), Some(0), "outside the group");
}

#[test]
fn setting_a_scene_stops_the_running_animation() {
    use mdc_protocol::SceneNode;
    let (host, mut device) = FakeDevice::pair(BoardProfile::default());
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

    // Install an atlas and play a clip.
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
    session.set_config("animation", "idle").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.current_atlas_frame().is_some());

    session
        .set_scene(SceneNode {
            name: "bg".into(),
            kind: "rect".into(),
            rect: Some([0, 0, 2, 2]),
            children: Vec::new(),
            value: Some("#FFFFFF".into()),
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
    assert_eq!(device.current_atlas_frame(), None, "scene took over");
    assert!(device.render_scene());
}

#[test]
fn mesh_preview_rasterizes_after_install() {
    let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    let package = mdc_model::DevicePackage {
        target: "linux-mesh".into(),
        mode: mdc_model::PackageMode::Mesh,
        nodes: Vec::new(),
        meshes: vec![mdc_model::Mesh {
            name: "tri".into(),
            vertices: 3,
            indices: 0,
            material: None,
            preview_xy: vec![(1, 1), (6, 1), (1, 6)],
            preview_indices: Vec::new(),
            ..mdc_model::Mesh::default()
        }],
        textures: Vec::new(),
        materials: Vec::new(),
        lights: Vec::new(),
        skeletons: Vec::new(),
        animations: Vec::new(),
        sprites: Vec::new(),
        layers: Vec::new(),
        estimated_bytes: 36,
        atlas: None,
        texture: None,
        texture_blobs: Vec::new(),
    };
    let sealed = mdc_model::MdcPackage::seal(package).unwrap();
    session
        .install_package(&sealed.to_bytes().unwrap())
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_preview());
    assert_ne!(device.pixel_rgb565(1, 1), Some(0));
}

#[test]
fn indexed_preview_does_not_draw_the_unindexed_corner() {
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 8,
        height: 8,
        renderer_mesh2d: true,
        renderer_max_vertices: 16,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    let package = mdc_model::DevicePackage {
        target: "linux-mesh".into(),
        mode: mdc_model::PackageMode::Mesh,
        nodes: Vec::new(),
        meshes: vec![mdc_model::Mesh {
            name: "tri".into(),
            vertices: 4,
            indices: 3,
            material: None,
            preview_xy: vec![(0, 0), (4, 0), (0, 4), (7, 7)],
            preview_indices: vec![0, 1, 2],
            ..mdc_model::Mesh::default()
        }],
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
    session
        .install_package(&sealed.to_bytes().unwrap())
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_preview());
    assert_ne!(device.pixel_rgb565(1, 1), Some(0));
    assert_eq!(device.pixel_rgb565(7, 7), Some(0));
}

#[test]
fn scene_sprite_blits_an_atlas_frame() {
    use mdc_protocol::SceneNode;
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 2,
        height: 2,
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
        .set_scene(SceneNode {
            name: "icon".into(),
            kind: "sprite".into(),
            rect: Some([0, 0, 1, 1]),
            children: Vec::new(),
            value: Some("0".into()),
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
    assert_eq!(device.pixel_rgb565(0, 0), Some(0xF800));
}

#[test]
fn sprite_fit_and_pad_share_the_image_box() {
    // N685: a sprite honours `fit`/`pad` like an image.
    use mdc_protocol::SceneNode;
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
            .set_scene(SceneNode {
                name: "icon".into(),
                kind: "sprite".into(),
                rect: Some([0, 0, 4, 4]),
                children: Vec::new(),
                value: Some("0".into()),
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
                value2: None,
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
    // Stretch fills the 4x4 rect.
    assert_eq!(lit("").len(), 16);
    // fit+pad=1 leaves a 2x2 box at (1,1).
    let padded = lit("fit,pad=1");
    assert_eq!(padded.len(), 4, "{padded:?}");
    assert!(padded
        .iter()
        .all(|(x, y)| (1..=2).contains(x) && (1..=2).contains(y)));
}

#[test]
fn sprite_flip_and_rotate_match_image_sampling() {
    // N695: sprite sampling applies the same flip/quarter-turn transforms.
    use mdc_protocol::SceneNode;
    let render = |mode: Option<&str>, rotate: Option<u16>| {
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 2,
            height: 1,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
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
            .set_scene(SceneNode {
                name: "icon".into(),
                kind: "sprite".into(),
                rect: Some([0, 0, 2, 1]),
                children: Vec::new(),
                value: Some("0".into()),
                value2: None,
                opacity: None,
                visible: None,
                anim: None,
                rotate,
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
        (
            device.pixel_rgb565(0, 0).unwrap(),
            device.pixel_rgb565(1, 0).unwrap(),
        )
    };
    assert_eq!(render(None, None), (0xF800, 0x001F));
    assert_eq!(render(Some("flip-h"), None), (0x001F, 0xF800));
    assert_eq!(render(None, Some(180)), (0x001F, 0xF800));
}

#[test]
fn sprite_bilinear_filter_blends_like_an_image() {
    // N705: a sprite honours the image `linear` token, so an upscaled atlas
    // frame blends between texels instead of repeating the nearest one.
    use mdc_protocol::SceneNode;
    let render = |mode: Option<&str>| {
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 1,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
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
            .set_scene(SceneNode {
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
    let nearest = render(None);
    assert_eq!(nearest, [0xF800, 0xF800, 0x001F, 0x001F]);
    let linear = render(Some("linear"));
    assert_eq!(linear[0], 0xF800, "the first texel is red");
    assert_eq!(linear[3], 0x001F, "the last texel is blue");
    assert_ne!(linear[1], 0xF800, "the interior blends");
    assert_ne!(linear[1], 0x001F, "the interior blends");
}

#[test]
fn sprite_fit_pad_bilinear_compose() {
    // N715: `fit`, `pad` and `linear` compose on a sprite: the letterbox is
    // inset, the draw box keeps the atlas aspect, and interior pixels blend.
    use mdc_protocol::SceneNode;
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 6,
        height: 6,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
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
        .set_scene(SceneNode {
            name: "icon".into(),
            kind: "sprite".into(),
            rect: Some([0, 0, 6, 6]),
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
            mode: Some("fit,pad=1,linear".into()),
            focus: None,
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_scene());
    // fit 2x1 into the 4x4 inner box -> 4x2 at (1,2); the corners stay clear.
    assert_eq!(device.pixel_rgb565(0, 0), Some(0));
    assert_eq!(device.pixel_rgb565(5, 5), Some(0));
    let left = device.pixel_rgb565(1, 2).unwrap();
    let right = device.pixel_rgb565(4, 2).unwrap();
    assert_eq!(left, 0xF800);
    assert_eq!(right, 0x001F);
    let (mid_left, mid_right) = (
        device.pixel_rgb565(2, 2).unwrap(),
        device.pixel_rgb565(3, 2).unwrap(),
    );
    assert_ne!(mid_left, 0xF800, "the interior blends");
    assert_ne!(mid_right, 0x001F, "the interior blends");
    // The row below is part of the same fitted box, not a repeat outside it.
    assert_ne!(device.pixel_rgb565(2, 3), Some(0));
    assert_eq!(device.pixel_rgb565(2, 4), Some(0));
}

#[test]
fn atlas_clip_playback_modes_and_speed() {
    // N730: ping-pong reverses at the ends; speed scales the frame clock;
    // `once` holds the last frame and reports no loops.
    let (_host, mut device) = FakeDevice::pair(BoardProfile {
        width: 1,
        height: 1,
        ..BoardProfile::default()
    });
    let clip = |mode: &str, speed: u16| mdc_model::AtlasClip {
        name: "walk".into(),
        start: 0,
        frames: 4,
        fps: 1,
        looping: true,
        fade_in_ms: 0,
        fade_out_ms: 0,
        mode: Some(mode.into()),
        speed_permille: Some(speed),
    };
    device.atlas = Some(mdc_model::AtlasBlob {
        sprite_width: 1,
        sprite_height: 1,
        frames: 4,
        columns: 4,
        rows: 1,
        rgb565: vec![0; 8],
        clips: vec![clip("ping-pong", 2000)],
        mips: Vec::new(),
    });
    device.anim_clip = Some("walk".into());
    device.anim_start_tick = 0;
    for (tick, expected) in [
        (0u64, 0u32),
        (500, 1),
        (1000, 2),
        (1500, 3),
        (2000, 2),
        (2500, 1),
        (3000, 0),
    ] {
        device.clock.tick = tick;
        assert_eq!(device.current_atlas_frame(), Some(expected), "tick {tick}");
    }
    device.clock.tick = 6000;
    assert_eq!(device.animation_loops(), 2, "two ping-pong cycles");

    // Once mode holds the last frame; speed is ignored past the end.
    if let Some(atlas) = device.atlas.as_mut() {
        atlas.clips = vec![clip("once", 1000)];
    }
    device.clock.tick = 10_000;
    assert_eq!(device.current_atlas_frame(), Some(3));
    assert_eq!(device.animation_loops(), 0);

    // A 2x loop reaches the same frame in half the time.
    if let Some(atlas) = device.atlas.as_mut() {
        atlas.clips = vec![clip("loop", 2000)];
    }
    device.anim_start_tick = 0;
    device.clock.tick = 500;
    assert_eq!(device.current_atlas_frame(), Some(1));
    device.clock.tick = 1000;
    assert_eq!(device.current_atlas_frame(), Some(2));
}

#[test]
fn atlas_fade_in_darkens_the_first_frame() {
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 1,
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
        estimated_bytes: 2,
        atlas: Some(mdc_model::AtlasBlob {
            sprite_width: 1,
            sprite_height: 1,
            frames: 1,
            columns: 1,
            rows: 1,
            rgb565: vec![0x00, 0xF8],
            clips: vec![mdc_model::AtlasClip {
                name: "idle".into(),
                start: 0,
                frames: 1,
                fps: 30,
                looping: true,
                fade_in_ms: 100,
                fade_out_ms: 0,
                mode: None,
                speed_permille: None,
            }],

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
    session.set_config("animation", "idle").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_atlas_frame());
    assert_eq!(device.pixel_rgb565(0, 0), Some(0));
    device.clock.advance(100);
    assert!(device.render_atlas_frame());
    assert_eq!(device.pixel_rgb565(0, 0), Some(0xF800));
}

#[test]
fn atlas_fade_out_darkens_the_last_moment() {
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 1,
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
        estimated_bytes: 2,
        atlas: Some(mdc_model::AtlasBlob {
            sprite_width: 1,
            sprite_height: 1,
            frames: 1,
            columns: 1,
            rows: 1,
            rgb565: vec![0x00, 0xF8],
            clips: vec![mdc_model::AtlasClip {
                name: "once".into(),
                start: 0,
                frames: 1,
                fps: 1,
                looping: false,
                fade_in_ms: 0,
                fade_out_ms: 400,
                mode: None,
                speed_permille: None,
            }],

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
    session.set_config("animation", "once").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_atlas_frame());
    assert_eq!(device.pixel_rgb565(0, 0), Some(0xF800));
    device.clock.advance(800);
    assert!(device.render_atlas_frame());
    assert_ne!(device.pixel_rgb565(0, 0), Some(0xF800));
}

#[test]
fn device_renders_the_scene_into_the_mirror() {
    use mdc_protocol::SceneNode;
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
            value: Some("#0000FF".into()),
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

    session.start_mirror("main", "full-frame", 15).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    device.emit_mirror_frame().unwrap();
    session.poll().unwrap();
    let frame = session.take_mirror_frame().unwrap();
    assert_eq!(&frame.bytes[0..2], &[0x1F, 0x00], "blue scene pixel");
}

#[test]
fn renderer_capability_follows_the_board_profile() {
    let (_, esp) = FakeDevice::with_default_profile();
    let renderer = esp.capabilities().renderer.unwrap();
    assert!(renderer.sprite_2d && !renderer.mesh2d && !renderer.skeleton);
    assert_eq!(renderer.max_texture, 512);

    let (_, gpu) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
    let renderer = gpu.capabilities().renderer.unwrap();
    assert!(renderer.mesh2d && renderer.mesh3d && renderer.skeleton);
    assert_eq!(renderer.max_texture, 2048);
    assert_eq!(renderer.max_vertices, 200_000);

    // N726: DQS is opt-in per board and selects the device render mode.
    let (_, dqs) = FakeDevice::pair(BoardProfile {
        renderer_dqs: true,
        ..BoardProfile::default()
    });
    assert!(dqs.capabilities().renderer.unwrap().dqs);
    assert_eq!(dqs.skin_mode(), mdc_bake::SkinMode::Dqs);
    let (_, lbs) = FakeDevice::with_default_profile();
    assert!(!lbs.capabilities().renderer.unwrap().dqs);
    assert_eq!(lbs.skin_mode(), mdc_bake::SkinMode::Lbs);
}

#[test]
fn accepts_a_runtime_package_and_reports_ready() {
    let profile = BoardProfile {
        width: 2,
        height: 2,
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

    let gltf = br#"{"asset":{"version":"2.0"},"nodes":[{"name":"root","mesh":0}],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
            "accessors":[{"count":4,"type":"VEC3"}]}"#;
    let model = mdc_model::GltfImporter
        .import(gltf, &mdc_model::ImportLimits::default())
        .unwrap();
    let package =
        mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::sprite_only())
            .unwrap();
    session
        .install_package(&package.to_bytes().unwrap())
        .unwrap();
    device.poll().unwrap();
    assert!(device.package_ready());
    assert_eq!(device.metrics.packages_received, 1);
}

#[test]
fn hello_capabilities_and_legacy_frame() {
    let profile = BoardProfile {
        width: 2,
        height: 2,
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    assert_eq!(
        session.device.as_ref().unwrap().capabilities.device_id,
        "sim-esp32-jc3248w535"
    );
    let pixels = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: pixels.clone(),
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    device.assert_pixels_eq(&pixels);
    assert_eq!(session.pending_request_count(), 0);
}

#[test]
fn accepts_chunked_frame_and_tile_with_base_check() {
    let profile = BoardProfile {
        width: 4,
        height: 2,
        device_id: "chunk-sim".into(),
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    // Force small chunk on device side via Hello negotiation: Session sends DEFAULT_MAX_CHUNK;
    // override after handshake by setting device.max_chunk before frame — Session uses caps.
    let mut session = Session::new(host);
    session.connect().unwrap();
    // Shrink device max_chunk before answering so capabilities advertise 8.
    device.max_chunk = 8;
    device.poll().unwrap();
    session.poll().unwrap();

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
    assert_eq!(device.current_frame_id(), 1);

    session
        .send_tile(Tile {
            surface_id: "main".into(),
            base_frame_id: 1,
            x: 1,
            y: 0,
            width: 1,
            height: 1,
            bytes: vec![0xaa, 0xbb],
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(&device.pixels()[2..4], &[0xaa, 0xbb]);

    // Bad base is rejected (Error); session surfaces protocol error on poll.
    session
        .send_tile(Tile {
            surface_id: "main".into(),
            base_frame_id: 99,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            bytes: vec![0, 0],
        })
        .unwrap();
    device.poll().unwrap();
    assert!(session.poll().is_err());
}

#[test]
fn fault_injector_drop_delay_disconnect() {
    let profile = BoardProfile {
        width: 2,
        height: 2,
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.faults.drop_next();
    device.poll().unwrap(); // HELLO dropped
    assert_ne!(session.poll().unwrap(), ConnectionState::Ready);

    // Re-send path: new session pair for delay + disconnect
    let (host, mut device) = FakeDevice::pair(BoardProfile {
        width: 2,
        height: 2,
        ..BoardProfile::default()
    });
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.faults.delay(5);
    device.poll().unwrap(); // HELLO delayed
    assert_ne!(session.poll().unwrap(), ConnectionState::Ready);
    device.advance(5).unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

    device.faults.request_disconnect();
    assert_eq!(device.poll(), Err(SimError::Disconnected));
}

#[test]
fn linux_virt_profile_matches_board_yaml() {
    let p = BoardProfile::from_board_id("linux-virt");
    assert_eq!(p.device_id, "sim-linux-virt");
    assert_eq!(p.width, 800);
    assert_eq!(p.height, 480);
    assert!(!p.touch);
    assert!(!p.ota);
    assert!(p.color_fps);
    let pi = BoardProfile::from_board_id("pi4-hdmi");
    assert_eq!((pi.width, pi.height), (640, 480));
    assert!(pi.color_fps);
    let eink = BoardProfile::from_board_id("waveshare-eink");
    assert!(!eink.color_fps, "e-ink must not claim color FPS");
}

#[test]
fn mirror_capability_follows_profile_and_gates_control() {
    // ESP32-S3 class: Form A only, no mirror capability.
    let (_, esp) = FakeDevice::pair(BoardProfile::default());
    assert!(!esp.capabilities().mirror.supports());
    assert_eq!(esp.capabilities().mirror_fps, 0);

    // A GPU board advertises mirror; local display and mirror FPS differ.
    let profile = BoardProfile {
        width: 4,
        height: 2,
        mirror: true,
        mirror_fps: 5,
        local_display_fps: 30,
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    let caps = device.capabilities();
    assert!(caps.mirror.supports());
    assert_eq!(caps.local_display_fps, 30);
    assert_eq!(caps.mirror_fps, 5);
    assert_ne!(caps.local_display_fps, caps.mirror_fps);
    assert!(caps.mirror_bandwidth > 0);

    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(session.supports_mirror());
    session.start_mirror("main", "full-frame", 30).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.mirror_active());

    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 4,
            height: 2,
            bytes: vec![7; 16],
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    // Device mirrors its rendered frame back to the host.
    device.emit_mirror_frame().unwrap();
    session.poll().unwrap();
    let mirrored = session.take_mirror_frame().unwrap();
    assert_eq!(mirrored.frame_id, 1);
    assert_eq!((mirrored.width, mirrored.height), (4, 2));
    assert_eq!(mirrored.bytes, vec![7; 16]);

    session.stop_mirror("main").unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(!device.mirror_active());
}

#[test]
fn device_masks_transparent_texels() {
    use mdc_model::{Material, Mesh, MeshGeometry, Node, TextureBlob};
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
        .install_package(
            &mdc_model::MdcPackage::seal(package)
                .unwrap()
                .to_bytes()
                .unwrap(),
        )
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert!(device.render_preview());
    assert_eq!(device.pixel_rgb565(2, 8), Some(0xF800), "opaque left drawn");
    assert_eq!(
        device.pixel_rgb565(13, 8),
        Some(0),
        "transparent right clear"
    );
}

#[test]
fn dirty_region_mirror_sends_only_changed_ranges() {
    use mdc_protocol::SceneNode;
    let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
    assert!(device.capabilities().mirror.supports_mode("dirty-region"));

    session
        .set_scene(SceneNode {
            name: "bg".into(),
            kind: "rect".into(),
            rect: Some([0, 0, 4, 4]),
            children: Vec::new(),
            value: Some("#000000".into()),
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
    session.start_mirror("main", "dirty-region", 10).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.mirror_mode(), "dirty-region");

    // First frame: no previous, sent in full.
    device.emit_mirror_frame().unwrap();
    session.poll().unwrap();
    let first = session.take_mirror_frame().expect("first frame");
    assert_eq!((first.width, first.height), (800, 480));

    // Change one small rect; the device chunks only the changed rows.
    session
        .set_scene(SceneNode {
            name: "bg".into(),
            kind: "rect".into(),
            rect: Some([0, 0, 4, 4]),
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
    device.emit_mirror_frame().unwrap();
    session.poll().unwrap();
    let second = session.take_mirror_frame().expect("patched frame");
    assert_eq!((second.width, second.height), (800, 480));
    assert_ne!(second.bytes, first.bytes, "the patch changed pixels");
}

#[test]
fn device_without_mirror_rejects_mirror_control() {
    let (host, mut esp) = FakeDevice::pair(BoardProfile::default());
    let mut session = Session::new(host);
    session.connect().unwrap();
    esp.poll().unwrap();
    session.poll().unwrap();
    assert!(!session.supports_mirror());
    assert_eq!(
        session.start_mirror("main", "full-frame", 30),
        Err(mdc_core::CoreError::Unsupported)
    );
    assert_eq!(
        session.stop_mirror("main"),
        Err(mdc_core::CoreError::Unsupported)
    );
    assert!(esp.emit_mirror_frame().is_err());
    assert!(!esp.mirror_active());
    // Nothing but the handshake reply ever reached the device.
    assert_eq!(esp.metrics.mirror_commands, 0);
}

#[test]
fn eink_busy_keeps_the_previous_frame() {
    let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("waveshare-eink"));
    assert_eq!(device.capabilities().max_fps, 0);
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    let first = vec![0x11; 800 * 480 * 2];
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 800,
            height: 480,
            bytes: first,
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.pixels()[0], 0x11);
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 800,
            height: 480,
            bytes: vec![0x22; 800 * 480 * 2],
        })
        .unwrap();
    device.poll().unwrap();
    assert_eq!(device.pixels()[0], 0x11);
    let err = session.poll().unwrap_err();
    assert!(err.to_string().contains("E_BUSY"), "{err}");
    device.advance(100).unwrap();
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 800,
            height: 480,
            bytes: vec![0x22; 800 * 480 * 2],
        })
        .unwrap();
    device.poll().unwrap();
    assert_eq!(device.pixels()[0], 0x22);
}

#[test]
fn default_profile_is_480x320() {
    let (_, device) = FakeDevice::with_default_profile();
    assert_eq!((device.profile.width, device.profile.height), (480, 320));
    assert_eq!(device.pixels().len(), 480 * 320 * 2);
}

#[test]
fn emit_input_reaches_session_manager() {
    use mdc_core::{DeviceManager, ManagerEvent};
    use std::sync::{Arc, Mutex};
    let profile = BoardProfile {
        width: 2,
        height: 2,
        touch: true,
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    let manager = Arc::new(Mutex::new(DeviceManager::new()));
    let mut session = Session::new(host);
    session.set_manager(manager.clone());
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    device
        .emit_input(InputEvent {
            surface_id: "main".into(),
            pointer_id: 0,
            phase: "down".into(),
            x: 1,
            y: 1,

            node: None,
        })
        .unwrap();
    session.poll().unwrap();
    let events: Vec<_> = manager.lock().unwrap().drain_events().collect();
    assert!(events.iter().any(|e| matches!(
        e,
        ManagerEvent::Input {
            event,
            ..
        } if event.phase == "down"
    )));
}

#[test]
fn scripted_input_follows_displayed_frame() {
    use mdc_core::{DeviceManager, Frame, ManagerEvent, Session};
    use std::sync::{Arc, Mutex};
    let profile = BoardProfile {
        width: 2,
        height: 2,
        touch: true,
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    device.emit_input_after_next_frame(InputEvent {
        surface_id: "main".into(),
        pointer_id: 1,
        phase: "down".into(),
        x: 1,
        y: 1,

        node: None,
    });
    let manager = Arc::new(Mutex::new(DeviceManager::new()));
    let mut session = Session::new(host);
    session.set_manager(manager.clone());
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![0; 8],
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    let events: Vec<_> = manager.lock().unwrap().drain_events().collect();
    assert!(events.iter().any(|event| matches!(
        event,
        ManagerEvent::Input { event, .. } if event.x == 1 && event.y == 1 && event.phase == "down"
    )));
}

#[test]
fn ota_gated_by_capability() {
    use mdc_protocol::OtaCommand;
    let profile = BoardProfile {
        width: 2,
        height: 2,
        ota: false,
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(
        session.send_ota(OtaCommand {
            action: "begin".into(),
            version: None,
            size: None,
            sha256: None,
            offset: None,
            len: None,
        }),
        Err(mdc_core::CoreError::Unsupported)
    );

    let profile = BoardProfile {
        width: 2,
        height: 2,
        ota: true,
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    session
        .send_ota(OtaCommand {
            action: "begin".into(),
            version: Some("1.0.0".into()),
            size: Some(10),
            sha256: None,
            offset: None,
            len: None,
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.metrics.ota_commands, 1);
}

fn ota(
    session: &mut Session<MemoryLink>,
    device: &mut FakeDevice,
    action: &str,
    version: Option<&str>,
    size: Option<u64>,
    sha: Option<&str>,
    expect_error: bool,
) {
    session
        .send_ota(OtaCommand {
            action: action.into(),
            version: version.map(str::to_string),
            size,
            sha256: sha.map(str::to_string),
            offset: None,
            len: None,
        })
        .unwrap();
    device.poll().unwrap();
    let polled = session.poll();
    if expect_error {
        assert!(polled.is_err());
    } else {
        polled.unwrap();
    }
}

#[test]
fn ota_failure_keeps_installed_firmware() {
    let profile = BoardProfile {
        width: 2,
        height: 2,
        ota: true,
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.firmware(), "1.0.0");

    ota(
        &mut session,
        &mut device,
        "begin",
        Some("9.9.9"),
        Some(2 * 1024 * 1024),
        None,
        true,
    );
    assert_eq!(device.firmware(), "1.0.0");
    assert!(device.pending_version().is_none());

    ota(
        &mut session,
        &mut device,
        "begin",
        Some("2.0.0"),
        Some(32),
        None,
        false,
    );
    assert_eq!(device.firmware(), "1.0.0");
    ota(
        &mut session,
        &mut device,
        "chunk",
        None,
        None,
        Some("bad"),
        true,
    );
    assert_eq!(device.firmware(), "1.0.0");
    assert!(device.metrics.ota_rollbacks >= 1);

    ota(
        &mut session,
        &mut device,
        "begin",
        Some("2.0.0"),
        Some(32),
        None,
        false,
    );
    ota(&mut session, &mut device, "abort", None, None, None, false);
    assert_eq!(device.firmware(), "1.0.0");

    ota(
        &mut session,
        &mut device,
        "begin",
        Some("2.0.0"),
        Some(32),
        None,
        false,
    );
    ota(&mut session, &mut device, "timeout", None, None, None, true);
    assert_eq!(device.firmware(), "1.0.0");

    ota(
        &mut session,
        &mut device,
        "begin",
        Some("2.0.0"),
        Some(32),
        None,
        false,
    );
    ota(
        &mut session,
        &mut device,
        "confirm",
        None,
        None,
        None,
        false,
    );
    assert_eq!(device.firmware(), "2.0.0");
}

#[test]
fn ota_resume_plan_skips_acknowledged_phases() {
    // N728: after begin+chunk the driver only confirms; a confirmed update
    // needs no further commands.
    let profile = BoardProfile {
        width: 2,
        height: 2,
        ota: true,
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    ota(
        &mut session,
        &mut device,
        "begin",
        Some("2.0.0"),
        Some(32),
        None,
        false,
    );
    ota(&mut session, &mut device, "chunk", None, None, None, false);
    assert_eq!(session.ota_phase(), 2);
    let plan = session.ota_resume_commands("2.0.0", Some(32), None);
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].action, "confirm");
    session.send_ota(plan[0].clone()).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.firmware(), "2.0.0");
    assert_eq!(session.ota_phase(), 3);
    assert!(session
        .ota_resume_commands("2.0.0", Some(32), None)
        .is_empty());
    // A fresh session still starts from begin.
    let (host, _device) = FakeDevice::pair(BoardProfile::default());
    let fresh = Session::new(host);
    assert_eq!(fresh.ota_resume_commands("3.0.0", None, None).len(), 3);
}

#[test]
fn ota_chunk_ranges_are_recorded_and_drive_resume() {
    // N737: a ranged chunk is recorded/merged; an out-of-image chunk rolls
    // the update back and clears the ranges.
    let profile = BoardProfile {
        width: 2,
        height: 2,
        ota: true,
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    ota(
        &mut session,
        &mut device,
        "begin",
        Some("2.0.0"),
        Some(12),
        None,
        false,
    );
    let chunk = |offset: u32, len: u32| mdc_protocol::OtaCommand {
        action: "chunk".into(),
        version: None,
        size: None,
        sha256: None,
        offset: Some(offset),
        len: Some(len),
    };
    session.send_ota(chunk(0, 4)).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.ota_received_ranges(), &[(0, 4)]);
    session.send_ota(chunk(4, 4)).unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.ota_received_ranges(), &[(0, 8)]);
    assert_eq!(
        Session::<MemoryLink>::ota_missing_chunks(12, 4, device.ota_received_ranges()),
        vec![(8, 4)]
    );
    session.send_ota(chunk(12, 4)).unwrap();
    device.poll().unwrap();
    assert!(session.poll().is_err());
    assert!(device.ota_received_ranges().is_empty());
    assert_eq!(device.firmware(), "1.0.0");
}

#[test]
fn ota_begin_freezes_pixels_until_abort() {
    use mdc_core::{Frame, Session};
    use mdc_protocol::OtaCommand;
    let profile = BoardProfile {
        width: 2,
        height: 2,
        ota: true,
        touch: false,
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    let painted = vec![0x11u8; 8];
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: painted.clone(),
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.pixels(), painted.as_slice());
    session
        .send_ota(OtaCommand {
            action: "begin".into(),
            version: Some("2.0.0".into()),
            size: Some(32),
            sha256: Some("0".repeat(64)),
            offset: None,
            len: None,
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![0x22; 8],
        })
        .unwrap();
    device.poll().unwrap();
    assert!(session.poll().is_err());
    assert_eq!(device.pixels(), painted.as_slice());
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
    device.poll().unwrap();
    session.poll().unwrap();
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![0x33; 8],
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.pixels(), &[0x33; 8]);
}

#[test]
fn dual_surface_accepts_overlay_and_rejects_unknown() {
    use mdc_core::{Frame, Session};
    let profile = BoardProfile::from_board_id("dual-sim");
    let (host, mut device) = FakeDevice::pair(profile);
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    let surfaces = &session.device.as_ref().unwrap().capabilities.surfaces;
    assert_eq!(surfaces.len(), 2);
    assert_eq!(surfaces[1].id, "overlay");
    let main_before = device.pixels().to_vec();
    session
        .send_frame(Frame {
            surface_id: "overlay".into(),
            width: 4,
            height: 2,
            bytes: vec![0xAB; 16],
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.overlay_pixels(), &[0xAB; 16]);
    assert_eq!(device.pixels(), main_before.as_slice());
    let missing = session.send_frame(Frame {
        surface_id: "nope".into(),
        width: 4,
        height: 2,
        bytes: vec![0; 16],
    });
    assert!(missing.is_err());
}

#[test]
fn overlay_tile_updates_overlay_and_rejects_out_of_bounds() {
    use mdc_core::{Frame, Session, Tile};
    let profile = BoardProfile::from_board_id("dual-sim");
    let (host, mut device) = FakeDevice::pair(profile);
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    let main_before = device.pixels().to_vec();
    session
        .send_frame(Frame {
            surface_id: "overlay".into(),
            width: 4,
            height: 2,
            bytes: vec![0xAB; 16],
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    session
        .send_tile(Tile {
            surface_id: "overlay".into(),
            base_frame_id: 1,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            bytes: vec![0x11, 0x22],
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(&device.overlay_pixels()[..2], &[0x11, 0x22]);
    assert_eq!(&device.overlay_pixels()[2..], &[0xAB; 14]);
    assert_eq!(device.pixels(), main_before.as_slice());
    let overlay_before = device.overlay_pixels().to_vec();
    let rejected = session.send_tile(Tile {
        surface_id: "overlay".into(),
        base_frame_id: 1,
        x: 4,
        y: 0,
        width: 1,
        height: 1,
        bytes: vec![0, 0],
    });
    assert_eq!(rejected, Err(mdc_core::CoreError::TileBounds));
    let payload = mdc_protocol::encode_tile_payload("overlay", 1, 4, 0, 1, 1, &[0, 0]).unwrap();
    device
        .handle_tile(mdc_protocol::Packet {
            version: mdc_protocol::VERSION,
            kind: mdc_protocol::MessageType::Tile,
            flags: 0,
            request_id: 99,
            payload,
        })
        .unwrap();
    let err = session.poll().unwrap_err();
    assert!(err.to_string().contains("E_DIM"), "{err}");
    assert_eq!(device.overlay_pixels(), overlay_before.as_slice());
    assert_eq!(device.pixels(), main_before.as_slice());
}

#[test]
fn corners_and_two_pointers_cancel_only_the_ones_still_down() {
    use mdc_core::{DeviceManager, ManagerEvent, Session};
    use std::sync::{Arc, Mutex};
    let profile = BoardProfile {
        width: 4,
        height: 2,
        touch: true,
        rotation: 90,
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    let manager = Arc::new(Mutex::new(DeviceManager::new()));
    let mut session = Session::new(host);
    session.set_manager(manager.clone());
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    let corners = [(0u16, 0u16), (3, 0), (0, 1), (3, 1)];
    for (x, y) in corners {
        let (tx, ty) = mdc_core::transform_touch(4, 2, 90, x, y).unwrap();
        device
            .emit_input(InputEvent {
                surface_id: "main".into(),
                pointer_id: 1,
                phase: "down".into(),
                x,
                y,

                node: None,
            })
            .unwrap();
        session.poll().unwrap();
        let events: Vec<_> = manager.lock().unwrap().drain_events().collect();
        assert!(
            events.iter().any(|event| matches!(
                event,
                ManagerEvent::Input { event, .. } if event.x == tx && event.y == ty
            )),
            "corner ({x},{y}) -> ({tx},{ty})"
        );
        device
            .emit_input(InputEvent {
                surface_id: "main".into(),
                pointer_id: 1,
                phase: "up".into(),
                x,
                y,

                node: None,
            })
            .unwrap();
        session.poll().unwrap();
        let _ = manager.lock().unwrap().drain_events().count();
    }
    assert!(mdc_core::transform_touch(4, 2, 90, 4, 0).is_err());
    device
        .emit_input(InputEvent {
            surface_id: "main".into(),
            pointer_id: 1,
            phase: "down".into(),
            x: 0,
            y: 0,

            node: None,
        })
        .unwrap();
    device
        .emit_input(InputEvent {
            surface_id: "main".into(),
            pointer_id: 2,
            phase: "down".into(),
            x: 1,
            y: 0,

            node: None,
        })
        .unwrap();
    device
        .emit_input(InputEvent {
            surface_id: "main".into(),
            pointer_id: 2,
            phase: "up".into(),
            x: 1,
            y: 0,

            node: None,
        })
        .unwrap();
    device.faults.request_disconnect();
    assert!(device.poll().is_err());
    session.poll().unwrap();
    let events: Vec<_> = manager.lock().unwrap().drain_events().collect();
    let cancels: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            ManagerEvent::Input { event, .. } if event.phase == "cancel" => Some(event.pointer_id),
            _ => None,
        })
        .collect();
    assert_eq!(cancels, vec![1]);
}

#[test]
fn rotation_transforms_touch_and_drops_out_of_bounds() {
    use mdc_core::{DeviceManager, ManagerEvent, Session};
    use std::sync::{Arc, Mutex};
    let profile = BoardProfile {
        width: 4,
        height: 2,
        touch: true,
        rotation: 90,
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    let manager = Arc::new(Mutex::new(DeviceManager::new()));
    let mut session = Session::new(host);
    session.set_manager(manager.clone());
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    device
        .emit_input(InputEvent {
            surface_id: "main".into(),
            pointer_id: 1,
            phase: "down".into(),
            x: 0,
            y: 0,

            node: None,
        })
        .unwrap();
    session.poll().unwrap();
    let events: Vec<_> = manager.lock().unwrap().drain_events().collect();
    assert!(events.iter().any(|event| matches!(
        event,
        ManagerEvent::Input { event, .. } if event.x == 1 && event.y == 0
    )));
    assert!(device
        .emit_input(InputEvent {
            surface_id: "main".into(),
            pointer_id: 1,
            phase: "down".into(),
            x: 4,
            y: 0,

            node: None,
        })
        .is_err());
}

#[test]
fn drop_next_keeps_previous_pixels() {
    use mdc_core::{Frame, Session};
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
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![0x11; 8],
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    device.faults.drop_next();
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![0x22; 8],
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.pixels(), &[0x11; 8]);
    assert_eq!(session.pending_request_count(), 1);
}

#[test]
fn linux_virt_is_800x480_and_rejects_early_tile() {
    use mdc_core::{Frame, Session, Tile};
    let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    let surface = &session.device.as_ref().unwrap().capabilities.surfaces[0];
    assert_eq!((surface.width, surface.height), (800, 480));
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
    device.poll().unwrap();
    assert!(session.poll().is_err());
    let bytes = vec![0x44u8; 800 * 480 * 2];
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 800,
            height: 480,
            bytes,
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    session
        .send_tile(Tile {
            surface_id: "main".into(),
            base_frame_id: 1,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            bytes: vec![0x44, 0x44],
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(session.pending_request_count(), 0);
}

#[test]
fn pending_frame_is_painted_only_after_the_inflight_ack() {
    use mdc_core::Frame;
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
    let send = |session: &mut Session<MemoryLink>, value: u8| {
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![value; 8],
            })
            .unwrap()
    };
    assert_ne!(send(&mut session, 0x11), 0);
    assert_eq!(send(&mut session, 0x22), 0);
    assert_eq!(send(&mut session, 0x33), 0);
    device.poll().unwrap();
    assert_eq!(device.pixels(), &[0x11; 8]);
    session.poll().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.pixels(), &[0x33; 8]);
}

#[test]
fn saturated_queue_rejects_a_tile() {
    use mdc_core::{CoreError, Tile};
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
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![1; 8],
        })
        .unwrap();
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![2; 8],
        })
        .unwrap();
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
        Err(CoreError::QueueFull)
    );
}

#[test]
fn disconnect_cancels_only_the_pointer_still_down() {
    use mdc_core::{DeviceManager, ManagerEvent};
    use std::sync::{Arc, Mutex};
    let profile = BoardProfile {
        width: 4,
        height: 2,
        touch: true,
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    let manager = Arc::new(Mutex::new(DeviceManager::new()));
    let mut session = Session::new(host);
    session.set_manager(manager.clone());
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    let down = |pointer_id, x, y| InputEvent {
        surface_id: "main".into(),
        pointer_id,
        phase: "down".into(),
        x,
        y,

        node: None,
    };
    device.emit_input(down(1, 0, 0)).unwrap();
    device.emit_input(down(2, 1, 0)).unwrap();
    device
        .emit_input(InputEvent {
            surface_id: "main".into(),
            pointer_id: 1,
            phase: "up".into(),
            x: 0,
            y: 0,

            node: None,
        })
        .unwrap();
    session.poll().unwrap();
    device.faults.request_disconnect();
    assert_eq!(device.poll(), Err(SimError::Disconnected));
    session.poll().unwrap();
    let events: Vec<_> = manager.lock().unwrap().drain_events().collect();
    let cancels: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            ManagerEvent::Input { event, .. } if event.phase == "cancel" => {
                Some((event.pointer_id, event.x, event.y))
            }
            _ => None,
        })
        .collect();
    assert_eq!(cancels, vec![(2, 1, 0)]);
}

#[test]
fn ota_virtual_timeout_unfreezes_without_changing_firmware() {
    let profile = BoardProfile {
        width: 2,
        height: 2,
        ota: true,
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    session
        .send_ota(OtaCommand {
            action: "begin".into(),
            version: Some("2.0.0".into()),
            size: Some(32),
            sha256: Some("0".repeat(64)),
            offset: None,
            len: None,
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    device.clock.advance(5_000);
    device.poll().unwrap();
    assert_eq!(device.firmware(), "1.0.0");
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![0x55; 8],
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.pixels(), &[0x55; 8]);
}

#[test]
fn dropped_commit_keeps_pixels_and_the_next_frame_displays() {
    let profile = BoardProfile {
        width: 2,
        height: 2,
        ..BoardProfile::default()
    };
    let (host, mut device) = FakeDevice::pair(profile);
    let mut session = Session::new(host);
    session.connect().unwrap();
    device.max_chunk = 8;
    device.poll().unwrap();
    session.poll().unwrap();
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![0x11; 8],
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.pixels(), &[0x11; 8]);
    // begin + chunk 放行，丢掉 commit。
    device.faults.drop_one_after(2);
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![0x22; 8],
        })
        .unwrap();
    device.poll().unwrap();
    let _ = session.poll();
    assert_eq!(device.pixels(), &[0x11; 8]);
    session
        .send_frame(Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![0x33; 8],
        })
        .unwrap();
    device.poll().unwrap();
    session.poll().unwrap();
    assert_eq!(device.pixels(), &[0x33; 8]);
}
