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
