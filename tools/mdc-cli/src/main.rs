//! MyDeviceCanvas CLI — sim-first host tooling (Apache-2.0).
use image::imageops::FilterType;
use mdc_core::{ConnectionState, DeviceManager, Frame, Session, Tile};
use mdc_discovery::{
    format_provision_log, CombinedDiscovery, DiscoveryProvider, Endpoint, MockMdnsProvider,
    PairingStore,
};
use mdc_flasher::{flags_for_board, EspToolFlasher, SimFlasher};
use mdc_mirror::MirrorSink;
use mdc_protocol::{InputEvent, OtaCommand, VERSION};
use mdc_provision::{Artifact, InstallPlan, Installer, PortLeases};
use mdc_simulator::{BoardProfile, FakeDevice, SimMetrics};
use mdc_transport::{MemoryLink, TcpTransport};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const MAX_SOURCE_DIMENSION: u32 = 4096;
const TARGET_WIDTH: u32 = 480;
const TARGET_HEIGHT: u32 = 320;
const DEFAULT_BENCHMARK_FRAMES: u32 = 30;

#[derive(Debug, Clone, Default)]
struct GlobalOpts {
    sim: bool,
    address: Option<String>,
    /// Explicit serial device path (AgentDeck-style candidate ports).
    port: Option<String>,
    board: String,
}

#[derive(Serialize)]
struct DeviceDto {
    device_id: String,
    firmware: String,
    surfaces: Vec<SurfaceDto>,
    state: String,
    /// Form B (N28): mirror + renderer capability so the inspector shows what
    /// the device can do without draining the mirror inbox.
    mirror: MirrorDto,
    renderer: Option<RendererDto>,
    /// N64: configuration keys and scene limits.
    #[serde(rename = "configKeys")]
    config_keys: Vec<String>,
    scene: SceneDto,
}

#[derive(Serialize)]
struct SceneDto {
    supported: bool,
    #[serde(rename = "maxNodes")]
    max_nodes: u32,
    #[serde(rename = "maxDepth")]
    max_depth: u32,
}

#[derive(Serialize)]
struct MirrorDto {
    supported: bool,
    modes: Vec<String>,
    max_fps: u16,
    mirror_fps: u16,
    local_display_fps: u16,
    bandwidth: u64,
}

#[derive(Serialize)]
struct RendererDto {
    sprite_2d: bool,
    mesh2d: bool,
    mesh3d: bool,
    skeleton: bool,
    max_texture: u32,
    max_vertices: u32,
}

#[derive(Serialize)]
struct SurfaceDto {
    id: String,
    width: u16,
    height: u16,
    pixel_format: String,
    stride: u32,
    rotation: u16,
}

fn load_rgb565(path: &Path, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let image = image::open(path).map_err(|error| format!("unable to decode image: {error}"))?;
    if image.width() > MAX_SOURCE_DIMENSION || image.height() > MAX_SOURCE_DIMENSION {
        return Err("source image exceeds 4096x4096 limit".into());
    }
    let image = image
        .resize_exact(width, height, FilterType::Triangle)
        .to_rgb8();
    let capacity = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(2))
        .ok_or("image size overflow")?;
    let mut output = Vec::with_capacity(capacity);
    for pixel in image.pixels() {
        let r = (u16::from(pixel[0]) >> 3) << 11;
        let g = (u16::from(pixel[1]) >> 2) << 5;
        let b = u16::from(pixel[2]) >> 3;
        let value = r | g | b;
        output.extend_from_slice(&value.to_le_bytes());
    }
    Ok(output)
}

fn parse_globals(args: &[String]) -> (GlobalOpts, Vec<String>) {
    let mut opts = GlobalOpts {
        board: "esp32-jc3248w535-sim".into(),
        ..GlobalOpts::default()
    };
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--sim" => opts.sim = true,
            "--address" => {
                i += 1;
                if i < args.len() {
                    opts.address = Some(args[i].clone());
                }
            }
            "--port" => {
                i += 1;
                if i < args.len() {
                    opts.port = Some(args[i].clone());
                }
            }
            "--board" => {
                i += 1;
                if i < args.len() {
                    opts.board = args[i].clone();
                }
            }
            other if other.starts_with("--address=") => {
                opts.address = Some(other["--address=".len()..].into());
            }
            other if other.starts_with("--port=") => {
                opts.port = Some(other["--port=".len()..].into());
            }
            other if other.starts_with("--board=") => {
                opts.board = other["--board=".len()..].into();
            }
            other => rest.push(other.into()),
        }
        i += 1;
    }
    (opts, rest)
}

fn devices_json(manager: &DeviceManager) -> String {
    let list: Vec<DeviceDto> = manager
        .devices()
        .map(|d| DeviceDto {
            device_id: d.capabilities.device_id.clone(),
            firmware: d.capabilities.firmware.clone(),
            state: format!("{:?}", d.state),
            surfaces: d
                .capabilities
                .surfaces
                .iter()
                .map(|s| SurfaceDto {
                    id: s.id.clone(),
                    width: s.width,
                    height: s.height,
                    pixel_format: s.pixel_format.clone(),
                    stride: s.stride,
                    rotation: s.rotation,
                })
                .collect(),
            mirror: MirrorDto {
                supported: d.capabilities.mirror.supports(),
                modes: d.capabilities.mirror.mode.clone(),
                max_fps: d.capabilities.mirror.max_fps,
                mirror_fps: d.capabilities.mirror_fps,
                local_display_fps: d.capabilities.local_display_fps,
                bandwidth: d.capabilities.mirror_bandwidth,
            },
            renderer: d.capabilities.renderer.as_ref().map(|r| RendererDto {
                sprite_2d: r.sprite_2d,
                mesh2d: r.mesh2d,
                mesh3d: r.mesh3d,
                skeleton: r.skeleton,
                max_texture: r.max_texture,
                max_vertices: r.max_vertices,
            }),
            config_keys: d.capabilities.config.keys.clone(),
            scene: SceneDto {
                supported: !d.capabilities.scene.is_disabled(),
                max_nodes: d.capabilities.scene.max_nodes,
                max_depth: d.capabilities.scene.max_depth,
            },
        })
        .collect();
    serde_json::to_string_pretty(&list).unwrap_or_else(|_| "[]".into())
}

fn inspect_json(manager: &DeviceManager, metrics: Option<&SimMetrics>) -> String {
    #[derive(Serialize)]
    struct Inspect<'a> {
        protocol: String,
        devices: Vec<DeviceDto>,
        metrics: Option<&'a SimMetrics>,
        image_bytes_included: bool,
        last_error: Option<String>,
    }
    let devices: Vec<DeviceDto> = manager
        .devices()
        .map(|d| DeviceDto {
            device_id: d.capabilities.device_id.clone(),
            firmware: d.capabilities.firmware.clone(),
            state: format!("{:?}", d.state),
            surfaces: d
                .capabilities
                .surfaces
                .iter()
                .map(|s| SurfaceDto {
                    id: s.id.clone(),
                    width: s.width,
                    height: s.height,
                    pixel_format: s.pixel_format.clone(),
                    stride: s.stride,
                    rotation: s.rotation,
                })
                .collect(),
            mirror: MirrorDto {
                supported: d.capabilities.mirror.supports(),
                modes: d.capabilities.mirror.mode.clone(),
                max_fps: d.capabilities.mirror.max_fps,
                mirror_fps: d.capabilities.mirror_fps,
                local_display_fps: d.capabilities.local_display_fps,
                bandwidth: d.capabilities.mirror_bandwidth,
            },
            renderer: d.capabilities.renderer.as_ref().map(|r| RendererDto {
                sprite_2d: r.sprite_2d,
                mesh2d: r.mesh2d,
                mesh3d: r.mesh3d,
                skeleton: r.skeleton,
                max_texture: r.max_texture,
                max_vertices: r.max_vertices,
            }),
            config_keys: d.capabilities.config.keys.clone(),
            scene: SceneDto {
                supported: !d.capabilities.scene.is_disabled(),
                max_nodes: d.capabilities.scene.max_nodes,
                max_depth: d.capabilities.scene.max_depth,
            },
        })
        .collect();
    serde_json::to_string_pretty(&Inspect {
        protocol: format!("{}.{}", VERSION.major, VERSION.minor),
        devices,
        metrics,
        image_bytes_included: false,
        last_error: None,
    })
    .unwrap_or_else(|_| "{}".into())
}

type SimSession = (Session<MemoryLink>, FakeDevice, Arc<Mutex<DeviceManager>>);

/// Handshake FakeDevice over MemoryLink; returns host session, peer, and shared manager.
fn open_sim(board: &str) -> Result<SimSession, Box<dyn std::error::Error>> {
    let profile = BoardProfile::from_board_id(board);
    let (host, mut device) = FakeDevice::pair(profile);
    let manager = Arc::new(Mutex::new(DeviceManager::new()));
    let mut session = Session::new(host);
    session.set_manager(manager.clone());
    session.connect()?;
    device.poll()?;
    if session.poll()? != ConnectionState::Ready {
        return Err("session did not become ready".into());
    }
    if let Some(dev) = session.device.as_ref() {
        manager
            .lock()
            .map_err(|e| e.to_string())?
            .connect_session(&dev.capabilities.device_id, "memory:sim");
    }
    Ok((session, device, manager))
}

fn pump_sim(
    session: &mut Session<MemoryLink>,
    device: &mut FakeDevice,
) -> Result<(), Box<dyn std::error::Error>> {
    device.poll()?;
    session.poll()?;
    Ok(())
}

fn cmd_simulate(opts: &GlobalOpts) -> Result<(), Box<dyn std::error::Error>> {
    let (mut session, mut device, manager) = open_sim(&opts.board)?;
    let w = device.profile().width;
    let h = device.profile().height;
    let bytes = vec![0u8; usize::from(w) * usize::from(h) * 2];
    session.send_frame(Frame {
        surface_id: "main".into(),
        width: w,
        height: h,
        bytes,
    })?;
    pump_sim(&mut session, &mut device)?;
    let metrics = device.metrics_snapshot();
    // Persist last metrics for inspector (best-effort local file).
    if let Ok(json) = serde_json::to_string_pretty(&metrics) {
        let path = std::env::temp_dir().join("mdc-last-sim-metrics.json");
        let _ = std::fs::write(&path, json);
    }
    println!("simulation ok: HELLO -> CAPABILITIES -> FRAME");
    println!("board={} device={}", opts.board, metrics.device_id);
    println!(
        "frames={} acks={} pixels={}",
        metrics.frames_received, metrics.acks_sent, metrics.pixel_bytes
    );
    let mgr = manager.lock().map_err(|e| e.to_string())?;
    println!("{}", devices_json(&mgr));
    Ok(())
}

fn with_sim_or_empty<F>(opts: &GlobalOpts, f: F) -> Result<(), Box<dyn std::error::Error>>
where
    F: FnOnce(&DeviceManager, Option<&SimMetrics>) -> Result<(), Box<dyn std::error::Error>>,
{
    if opts.sim || opts.address.is_none() {
        // Default to sim path when no live address (honest sim-first CLI).
        let (session, device, manager) = open_sim(&opts.board)?;
        let metrics = device.metrics_snapshot();
        drop(session);
        drop(device);
        let mgr = manager.lock().map_err(|e| e.to_string())?;
        return f(&mgr, Some(&metrics));
    }
    // Address without --sim: empty until a live peer is connected.
    let empty = DeviceManager::new();
    f(&empty, None)
}

fn cmd_devices(opts: &GlobalOpts) -> Result<(), Box<dyn std::error::Error>> {
    with_sim_or_empty(opts, |mgr, _| {
        println!("{}", devices_json(mgr));
        Ok(())
    })
}

fn cmd_inspect(opts: &GlobalOpts, extra: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    // N57: bounded live stream (`--watch --count N --interval MS`).
    let mut watch = false;
    let mut count: u32 = 3;
    let mut interval_ms: u64 = 200;
    let mut iter = extra.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--watch" => watch = true,
            "--count" => count = iter.next().and_then(|v| v.parse().ok()).unwrap_or(count),
            "--interval" => {
                interval_ms = iter
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(interval_ms)
            }
            _ => {}
        }
    }
    if watch {
        let (mut session, mut device, manager) = open_sim(&opts.board)?;
        for tick in 0..count {
            let _ = session.poll();
            let _ = device.poll();
            let snapshot = device.metrics_snapshot();
            let mgr = manager.lock().map_err(|e| e.to_string())?;
            println!(
                "{}",
                serde_json::json!({
                    "tick": tick,
                    "board": opts.board,
                    "scene_nodes": device.scene_node_count(),
                    "atlas_frames": device.atlas_frames(),
                    "config": inspect_json(&mgr, Some(&snapshot)),
                })
            );
            if tick + 1 < count {
                std::thread::sleep(std::time::Duration::from_millis(interval_ms));
            }
        }
        return Ok(());
    }
    with_sim_or_empty(opts, |mgr, metrics| {
        println!("{}", inspect_json(mgr, metrics));
        Ok(())
    })
}

/// N32: compile a source model into a runtime package for a board's renderer.
fn cmd_package(
    opts: &GlobalOpts,
    model_path: &str,
    out: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read(model_path)?;
    let model = if model_path.to_ascii_lowercase().ends_with(".json") {
        mdc_model::Live2DImporter.import(&bytes, &mdc_model::ImportLimits::default())?
    } else {
        mdc_model::GltfImporter.import(&bytes, &mdc_model::ImportLimits::default())?
    };
    let profile = BoardProfile::from_board_id(&opts.board);
    let caps = mdc_model::RendererCapability {
        sprite: profile.renderer_sprite,
        mesh2d: profile.renderer_mesh2d,
        skeleton: profile.renderer_skeleton,
        max_texture: profile.renderer_max_texture,
        max_vertices: profile.renderer_max_vertices,
    };
    let package = mdc_model::compile_to_package(&model, &caps)?;
    let bytes = package.to_bytes()?;
    let out = out.unwrap_or_else(|| format!("{model_path}.mdcpkg"));
    std::fs::write(&out, &bytes)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "board": opts.board,
            "source": model_path,
            "target": package.manifest.target,
            "mode": package.manifest.mode,
            "package_bytes": bytes.len(),
            "output": out,
        }))?
    );
    Ok(())
}

/// N32: upload a runtime package to a simulator board and report readiness.
fn cmd_install_package(opts: &GlobalOpts, path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    let (mut session, mut device, _manager) = open_sim(&opts.board)?;
    session.install_package(&bytes)?;
    device.poll()?;
    session.poll()?;
    let ready = device.package_ready();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "board": opts.board,
            "package": path,
            "package_bytes": bytes.len(),
            "renderer_ready": ready,
            "packages_received": device.metrics.packages_received,
        }))?
    );
    if !ready {
        return Err("device did not accept the runtime package".into());
    }
    Ok(())
}
/// N55: set a scene from a JSON file, or clear it, on a simulator board.
fn cmd_scene(opts: &GlobalOpts, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let (mut session, mut device, _manager) = open_sim(&opts.board)?;
    let mut action = "set";
    if args.first().map(String::as_str) == Some("clear") {
        session.clear_scene()?;
        action = "clear";
    } else {
        let path = args
            .first()
            .ok_or("scene requires a JSON file or 'clear'")?;
        let text = std::fs::read_to_string(path)?;
        let node: mdc_protocol::SceneNode = serde_json::from_str(&text)?;
        session.set_scene(node)?;
    }
    device.poll()?;
    session.poll()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "board": opts.board,
            "action": action,
            "scene_nodes": device.scene_node_count(),
            "present": device.has_scene(),
        }))?
    );
    Ok(())
}

/// N52: bake source images into a device sprite atlas (pixels + manifest).
fn cmd_bake(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut images: Vec<String> = Vec::new();
    let mut frames: u32 = 4;
    let mut sprite: u32 = 64;
    let mut max_texture: u32 = 2048;
    let mut out_dir = String::from(".");
    let mut package_path: Option<String> = None;
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--frames" => frames = iter.next().and_then(|v| v.parse().ok()).unwrap_or(frames),
            "--sprite" => sprite = iter.next().and_then(|v| v.parse().ok()).unwrap_or(sprite),
            "--max-texture" => {
                max_texture = iter
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(max_texture)
            }
            "--out" => out_dir = iter.next().cloned().unwrap_or(out_dir),
            "--package" => package_path = iter.next().cloned(),
            other => images.push(other.to_string()),
        }
    }
    if images.is_empty() {
        return Err("bake requires at least one source image".into());
    }
    let mut sources = Vec::with_capacity(images.len());
    for path in &images {
        let bytes = std::fs::read(path)?;
        let name = Path::new(path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("source")
            .to_string();
        sources.push(mdc_bake::SpriteSource { name, image: bytes });
    }
    // A synthetic motion whose frame_count is exactly `frames`.
    let fps = 30.0;
    let duration = if frames > 1 {
        f64::from(frames - 1) / fps
    } else {
        0.0
    };
    let motions = vec![mdc_model::MotionInfo {
        name: "bake".into(),
        channels: 1,
        duration_sec: duration,
        fps,
        fade_in_sec: 0.0,
        fade_out_sec: 0.0,
        looping: true,
    }];
    let package = mdc_bake::bake_sprites(
        &sources,
        &motions,
        (sprite, sprite),
        max_texture,
        &mdc_bake::BakeLimits::default(),
    )?;
    if let Some(path) = &package_path {
        // N62: write a runtime package instead of (or as well as) preview files.
        let sealed = mdc_bake::bake_to_package(
            &sources,
            &motions,
            (sprite, sprite),
            max_texture,
            &mdc_bake::BakeLimits::default(),
        )?;
        let bytes = sealed.to_bytes().map_err(|e| format!("package: {e}"))?;
        std::fs::write(path, &bytes)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "sources": images.len(),
                "frames": package.atlas.frames,
                "mode": sealed.manifest.mode,
                "package_path": path,
                "package_bytes": bytes.len(),
            }))?
        );
        return Ok(());
    }
    std::fs::create_dir_all(&out_dir)?;
    let atlas_path = Path::new(&out_dir).join("atlas.ppm");
    std::fs::write(&atlas_path, rgb565_to_ppm(&package.atlas))?;
    let manifest_path = Path::new(&out_dir).join("atlas.json");
    std::fs::write(&manifest_path, package.manifest.to_json()?)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "sources": images.len(),
            "frames": package.atlas.frames,
            "atlas": format!("{}x{}", package.atlas.width, package.atlas.height),
            "atlas_path": atlas_path.display().to_string(),
            "manifest_path": manifest_path.display().to_string(),
        }))?
    );
    Ok(())
}

/// N112/N117: bake a skinned model's poses into sprite frames (or a runtime
/// package) using linear blend skinning. `--texture <png>` samples the mesh UVs
/// from a texture instead of filling a solid colour.
fn cmd_bake_mesh(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut model_path: Option<String> = None;
    let mut frames: u32 = 8;
    let mut sprite: u32 = 64;
    let mut animation: usize = 0;
    let mut out_dir = String::from(".");
    let mut package_path: Option<String> = None;
    let mut texture_path: Option<String> = None;
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--frames" => frames = iter.next().and_then(|v| v.parse().ok()).unwrap_or(frames),
            "--sprite" => sprite = iter.next().and_then(|v| v.parse().ok()).unwrap_or(sprite),
            "--animation" => {
                animation = iter
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(animation)
            }
            "--out" => out_dir = iter.next().cloned().unwrap_or(out_dir),
            "--package" => package_path = iter.next().cloned(),
            "--texture" => texture_path = iter.next().cloned(),
            other => model_path = Some(other.to_string()),
        }
    }
    let model_path = model_path.ok_or("bake-mesh requires a .gltf/.glb model")?;
    let bytes = std::fs::read(&model_path)?;
    let model = mdc_model::GltfImporter.import(&bytes, &mdc_model::ImportLimits::default())?;
    let has_uv = model.meshes.iter().any(|m| !m.preview_uv.is_empty());
    let morph_targets = model
        .meshes
        .iter()
        .map(|m| m.morph_targets.len())
        .max()
        .unwrap_or(0);
    let texture = match &texture_path {
        Some(path) => {
            let image = std::fs::read(path)?;
            let baked = mdc_bake::decode_rgb565_native(&image, &mdc_bake::BakeLimits::default())?;
            Some(mdc_bake::TextureMap::from_baked(&baked)?)
        }
        None => None,
    };
    let textured = texture.is_some() && has_uv;
    if let Some(path) = &package_path {
        let sealed = mdc_bake::bake_textured_to_package(
            &model,
            animation,
            frames,
            (sprite, sprite),
            if textured { texture.as_ref() } else { None },
            0xF800,
        )?;
        let package_bytes = sealed.to_bytes().map_err(|e| format!("package: {e}"))?;
        std::fs::write(path, &package_bytes)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "source": model_path,
                "frames": frames,
                "mode": sealed.manifest.mode,
                "skinned": true,
                "textured": textured,
                "morphTargets": morph_targets,
                "package_path": path,
                "package_bytes": package_bytes.len(),
            }))?
        );
        return Ok(());
    }
    let baked = mdc_bake::bake_textured_mesh(
        &model,
        animation,
        frames,
        (sprite, sprite),
        if textured { texture.as_ref() } else { None },
        0xF800,
    )?;
    std::fs::create_dir_all(&out_dir)?;
    let mut written = Vec::new();
    for (index, frame) in baked.iter().enumerate() {
        let path = Path::new(&out_dir).join(format!("mesh_frame_{index:02}.ppm"));
        std::fs::write(
            &path,
            rgb565_frame_to_ppm(frame.width, frame.height, &frame.rgb565),
        )?;
        written.push(path.display().to_string());
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "source": model_path,
            "frames": baked.len(),
            "sprite": format!("{sprite}x{sprite}"),
            "skinned": true,
            "textured": textured,
            "morphTargets": morph_targets,
            "files": written,
        }))?
    );
    Ok(())
}

/// Encode a baked RGB565 atlas as a binary PPM (P6).
fn rgb565_to_ppm(atlas: &mdc_bake::BakedAtlas) -> Vec<u8> {
    rgb565_frame_to_ppm(atlas.width, atlas.height, &atlas.rgb565)
}

/// Encode a tightly packed RGB565 frame as a binary PPM (P6).
fn rgb565_frame_to_ppm(width: u32, height: u32, rgb565: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgb565.len() / 2 * 3 + 32);
    out.extend_from_slice(format!("P6\n{width} {height}\n255\n").as_bytes());
    for chunk in rgb565.chunks_exact(2) {
        let value = u16::from_le_bytes([chunk[0], chunk[1]]);
        let r = ((value >> 11) & 0x1f) as u8;
        let g = ((value >> 5) & 0x3f) as u8;
        let b = (value & 0x1f) as u8;
        out.push((r << 3) | (r >> 2));
        out.push((g << 2) | (g >> 4));
        out.push((b << 3) | (b >> 2));
    }
    out
}

/// N134: render one posed frame of a model (optionally textured) to a PPM.
fn cmd_render_mesh(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut model_path: Option<String> = None;
    let mut texture_path: Option<String> = None;
    let mut out: Option<String> = None;
    let mut animation: usize = 0;
    let mut time: f32 = 0.0;
    let mut sprite: u32 = 64;
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--texture" => texture_path = iter.next().cloned(),
            "--out" => out = iter.next().cloned(),
            "--animation" => {
                animation = iter
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(animation)
            }
            "--time" => time = iter.next().and_then(|v| v.parse().ok()).unwrap_or(time),
            "--sprite" => sprite = iter.next().and_then(|v| v.parse().ok()).unwrap_or(sprite),
            other => model_path = Some(other.to_string()),
        }
    }
    let model_path = model_path.ok_or("render-mesh requires a .gltf/.glb model")?;
    let out = out.ok_or("render-mesh requires --out <file.ppm>")?;
    let bytes = std::fs::read(&model_path)?;
    let model = mdc_model::GltfImporter.import(&bytes, &mdc_model::ImportLimits::default())?;
    let has_uv = model.meshes.iter().any(|m| !m.preview_uv.is_empty());
    let texture = match &texture_path {
        Some(path) => {
            let image = std::fs::read(path)?;
            let baked = mdc_bake::decode_rgb565_native(&image, &mdc_bake::BakeLimits::default())?;
            Some(mdc_bake::TextureMap::from_baked(&baked)?)
        }
        None => None,
    };
    let textured = texture.is_some() && has_uv;
    let has_geometry = model.meshes.iter().any(|m| m.geometry.is_some());
    let frame = mdc_bake::render_geometry_at(
        &model,
        animation,
        time,
        (sprite, sprite),
        if textured { texture.as_ref() } else { None },
        0xF800,
    )?;
    std::fs::write(
        &out,
        rgb565_frame_to_ppm(frame.width, frame.height, &frame.rgb565),
    )?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "source": model_path,
            "animation": animation,
            "time": time,
            "sprite": format!("{sprite}x{sprite}"),
            "textured": textured,
            "geometry": has_geometry,
            "output": out,
        }))?
    );
    Ok(())
}

/// N135: export one frame of a baked runtime package's atlas as a PPM.
fn cmd_render_package(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut file: Option<String> = None;
    let mut out: Option<String> = None;
    let mut frame: u32 = 0;
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--out" => out = iter.next().cloned(),
            "--frame" => frame = iter.next().and_then(|v| v.parse().ok()).unwrap_or(frame),
            other => file = Some(other.to_string()),
        }
    }
    let file = file.ok_or("render-package requires a .mdcpkg file")?;
    let out = out.ok_or("render-package requires --out <file.ppm>")?;
    let bytes = std::fs::read(&file)?;
    let package = mdc_model::MdcPackage::from_bytes(&bytes)?;
    let atlas = package
        .package
        .atlas
        .as_ref()
        .ok_or("package has no baked atlas to render")?;
    let offset = atlas
        .frame_offset(frame)
        .ok_or_else(|| format!("frame {frame} out of range ({} frames)", atlas.frames))?;
    let sprite_w = atlas.sprite_width as usize;
    let sprite_h = atlas.sprite_height as usize;
    let atlas_row = (atlas.columns as usize) * sprite_w * 2;
    let mut frame_pixels = vec![0u8; sprite_w * sprite_h * 2];
    for y in 0..sprite_h {
        let src = offset + y * atlas_row;
        let dst = y * sprite_w * 2;
        if src + sprite_w * 2 > atlas.rgb565.len() {
            break;
        }
        frame_pixels[dst..dst + sprite_w * 2]
            .copy_from_slice(&atlas.rgb565[src..src + sprite_w * 2]);
    }
    std::fs::write(
        &out,
        rgb565_frame_to_ppm(atlas.sprite_width, atlas.sprite_height, &frame_pixels),
    )?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "package": file,
            "frame": frame,
            "frames": atlas.frames,
            "sprite": format!("{}x{}", atlas.sprite_width, atlas.sprite_height),
            "output": out,
        }))?
    );
    Ok(())
}

/// N82: render a scene on a simulator device and export the framebuffer as PPM.
fn cmd_scene_render(opts: &GlobalOpts, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut file: Option<String> = None;
    let mut out: Option<String> = None;
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--out" => out = iter.next().cloned(),
            other => file = Some(other.to_string()),
        }
    }
    let file = file.ok_or("scene-render requires a scene JSON file")?;
    let out = out.ok_or("scene-render requires --out <file.ppm>")?;
    let text = std::fs::read_to_string(&file)?;
    let node: mdc_protocol::SceneNode = serde_json::from_str(&text)?;
    let (mut session, mut device, _manager) = open_sim(&opts.board)?;
    session.set_scene(node)?;
    device.poll()?;
    session.poll()?;
    let drew = device.render_scene();
    let profile = BoardProfile::from_board_id(&opts.board);
    let pixels = device.framebuffer();
    std::fs::write(
        &out,
        rgb565_frame_to_ppm(u32::from(profile.width), u32::from(profile.height), pixels),
    )?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "board": opts.board,
            "drew": drew,
            "output": out,
        }))?
    );
    Ok(())
}

/// N42: read or write a device configuration value on a simulator board.
fn cmd_config(
    opts: &GlobalOpts,
    action: &str,
    key: &str,
    value: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut session, mut device, _manager) = open_sim(&opts.board)?;
    match action {
        "get" => {
            session.get_config(key)?;
        }
        "set" => {
            let value = value.ok_or("set requires a value")?;
            session.set_config(key, value)?;
        }
        other => return Err(format!("unknown config action {other}").into()),
    }
    device.poll()?;
    session.poll()?;
    let learned = session.config(key).map(str::to_string);
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "board": opts.board,
            "key": key,
            "value": learned,
            "ok": learned.is_some(),
        }))?
    );
    Ok(())
}

/// Form B (N31): start mirroring on a simulator board, drive a fixed number of
/// frames, and deliver them to a file watcher (`--write-dir`) and/or a TCP
/// consumer (`--serve`). Prints JSON stats.
fn cmd_mirror(opts: &GlobalOpts, extra: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut frames = 3u32;
    let mut write_dir: Option<String> = None;
    let mut serve: Option<String> = None;
    let mut http: Option<String> = None;
    let mut format = String::from("ppm");
    let mut iter = extra.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--frames" => {
                frames = iter.next().and_then(|v| v.parse().ok()).unwrap_or(frames);
            }
            "--write-dir" => write_dir = iter.next().cloned(),
            "--serve" => serve = iter.next().cloned(),
            "--http" => http = iter.next().cloned(),
            "--format" => format = iter.next().cloned().unwrap_or(format),
            _ => {}
        }
    }
    let (mut session, mut device, _manager) = open_sim(&opts.board)?;
    if !session.supports_mirror() {
        return Err(format!("board {} does not advertise mirror", opts.board).into());
    }
    session.start_mirror("main", "full-frame", 15)?;
    device.poll()?;
    session.poll()?;

    let mut sink = match (&write_dir, format.as_str()) {
        (Some(dir), "ppm") => Some(mdc_mirror::PpmDirectorySink::new(dir, "frame.ppm", 0)),
        _ => None,
    };
    let bmp_dir = match (&write_dir, format.as_str()) {
        (Some(dir), "bmp") => Some(PathBuf::from(dir)),
        _ => None,
    };
    if let Some(dir) = &bmp_dir {
        std::fs::create_dir_all(dir)?;
    }
    let mut server = match &serve {
        Some(address) => Some(mdc_mirror::MirrorServer::bind(address)?),
        None => None,
    };
    let mut http_server = match &http {
        Some(address) => Some(mdc_mirror::HttpMirrorServer::bind(address)?),
        None => None,
    };
    if let Some(server) = &http_server {
        println!("http mirror on {}", server.local_addr()?);
    }

    let mut frames_seen = 0u64;
    for _ in 0..frames {
        device.emit_mirror_frame()?;
        session.poll()?;
        let Some(frame) = session.take_mirror_frame() else {
            continue;
        };
        frames_seen += 1;
        let shadow = |generation: u64| mdc_mirror::ShadowFrame {
            width: frame.width,
            height: frame.height,
            generation,
            frame_id: frame.frame_id,
            pixels: &frame.bytes,
        };
        if let Some(sink) = sink.as_mut() {
            sink.on_frame(shadow(frames_seen));
        }
        if let Some(dir) = &bmp_dir {
            let bmp = mdc_mirror::encode_bmp(frame.width, frame.height, &frame.bytes);
            std::fs::write(dir.join("frame.bmp"), bmp)?;
        }
        if let Some(server) = server.as_mut() {
            let _ = server.accept_pending();
            server.publish(&shadow(frames_seen));
            server.flush();
        }
        if let Some(server) = http_server.as_mut() {
            server.publish(&shadow(frames_seen));
            let _ = server.pump();
        }
    }
    let stats = session.mirror_stats();
    let out = serde_json::json!({
        "board": opts.board,
        "frames_requested": frames,
        "frames_seen": frames_seen,
        "mirror_received": stats.frames_received,
        "mirror_dropped": stats.frames_dropped,
        "ppm_written": sink.as_ref().map(|s| s.frames_written()),
        "served_clients": server.as_ref().map(|s| s.client_count()),
        "served_dropped": server.as_ref().map(|s| s.frames_dropped()),
        "http_frames": http_server.as_ref().map(|s| s.frames_published()),
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

#[cfg(feature = "serial")]
fn open_serial_session(
    port: &str,
) -> Result<Session<mdc_transport::SerialTransport>, Box<dyn std::error::Error>> {
    use mdc_transport::{is_candidate_serial_port, SerialConfig, SerialOpenGuard, SerialTransport};
    if !is_candidate_serial_port(port) {
        return Err(format!("port {port} does not look like an ESP32 USB-serial candidate").into());
    }
    let leases = PortLeases::default();
    leases
        .session_may_open(port, 0)
        .map_err(|error| error.to_string())?;
    let mut guard = SerialOpenGuard::default();
    let gen = guard.generation;
    let config = SerialConfig {
        baud: 115_200,
        timeout: Duration::from_millis(100),
        reset_on_open: false,
    };
    let transport = match SerialTransport::open_with_config(port, config) {
        Ok(t) => {
            guard.note_success();
            t
        }
        Err(e) => {
            let permanent =
                e.to_string().contains("Permission") || e.to_string().contains("Access");
            let backoff = guard.note_failure(permanent);
            return Err(
                format!("serial open failed ({e}); retry after {backoff}ms (gen={gen})").into(),
            );
        }
    };
    if guard.is_stale(gen) {
        return Err("serial open raced with cancel".into());
    }
    let mut session = Session::new(transport);
    session.connect()?;
    for _ in 0..50 {
        if session.poll()? == ConnectionState::Ready {
            return Ok(session);
        }
    }
    Err("serial peer did not complete handshake".into())
}

fn cmd_send_image(opts: &GlobalOpts, path: &str) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "serial")]
    if let Some(port) = &opts.port {
        let bytes = load_rgb565(Path::new(path), TARGET_WIDTH, TARGET_HEIGHT)?;
        let mut session = open_serial_session(port)?;
        let (w, h) = session
            .device
            .as_ref()
            .and_then(|d| d.capabilities.surfaces.first())
            .map(|s| (s.width, s.height))
            .unwrap_or((TARGET_WIDTH as u16, TARGET_HEIGHT as u16));
        let bytes = if w as u32 == TARGET_WIDTH && h as u32 == TARGET_HEIGHT {
            bytes
        } else {
            load_rgb565(Path::new(path), w as u32, h as u32)?
        };
        session.send_frame(Frame {
            surface_id: "main".into(),
            width: w,
            height: h,
            bytes,
        })?;
        println!("sent frame to serial {port}");
        return Ok(());
    }
    #[cfg(not(feature = "serial"))]
    if opts.port.is_some() {
        return Err("this build was compiled without the serial feature".into());
    }
    if let Some(addr) = &opts.address {
        let bytes = load_rgb565(Path::new(path), TARGET_WIDTH, TARGET_HEIGHT)?;
        let transport = TcpTransport::connect(addr.as_str(), Duration::from_secs(2))?;
        let mut session = Session::new(transport);
        session.connect()?;
        // Live peer must answer capabilities; poll briefly.
        for _ in 0..20 {
            if session.poll()? == ConnectionState::Ready {
                break;
            }
        }
        if session.state != ConnectionState::Ready {
            return Err("tcp peer did not complete handshake".into());
        }
        let device_id = session
            .device
            .as_ref()
            .map(|d| d.capabilities.device_id.clone())
            .unwrap_or_default();
        let (w, h) = session
            .device
            .as_ref()
            .and_then(|d| d.capabilities.surfaces.first())
            .map(|s| (s.width, s.height))
            .unwrap_or((TARGET_WIDTH as u16, TARGET_HEIGHT as u16));
        let bytes = if w as u32 == TARGET_WIDTH && h as u32 == TARGET_HEIGHT {
            bytes
        } else {
            load_rgb565(Path::new(path), w as u32, h as u32)?
        };
        send_authorized_tcp_frame(
            &mut session,
            &pairing_path(),
            addr,
            Frame {
                surface_id: "main".into(),
                width: w,
                height: h,
                bytes,
            },
        )?;
        println!("sent frame to {addr} device_id={device_id}");
        return Ok(());
    }
    if !opts.sim {
        eprintln!("send-image: use --sim or --address host:port");
        std::process::exit(2);
    }
    let (mut session, mut device, _) = open_sim(&opts.board)?;
    let w = device.profile().width;
    let h = device.profile().height;
    let bytes = load_rgb565(Path::new(path), w as u32, h as u32)?;
    session.send_frame(Frame {
        surface_id: "main".into(),
        width: w,
        height: h,
        bytes,
    })?;
    pump_sim(&mut session, &mut device)?;
    println!(
        "sent RGB565 {}x{} to sim device {}",
        w,
        h,
        device.profile().device_id
    );
    Ok(())
}

fn cmd_benchmark(opts: &GlobalOpts, frames: u32) -> Result<(), Box<dyn std::error::Error>> {
    if opts.address.is_some() && !opts.sim {
        eprintln!("benchmark over TCP requires a live peer; use --sim for local FakeDevice");
        std::process::exit(2);
    }
    let (mut session, mut device, _) = open_sim(&opts.board)?;
    let w = device.profile().width;
    let h = device.profile().height;
    let pixel_len = usize::from(w) * usize::from(h) * 2;
    let mut sent = 0u32;
    let mut acked = 0u32;
    for i in 0..frames {
        let mut bytes = vec![0u8; pixel_len];
        if !bytes.is_empty() {
            bytes[0] = (i & 0xff) as u8;
        }
        session.send_frame(Frame {
            surface_id: "main".into(),
            width: w,
            height: h,
            bytes,
        })?;
        sent += 1;
        pump_sim(&mut session, &mut device)?;
        if session.pending_request_count() == 0 {
            acked += 1;
        }
    }
    let metrics = device.metrics_snapshot();
    let timings = session.displayed_timings();
    let count = timings.len().max(1) as u128;
    let transport_us = timings
        .iter()
        .map(|sample| sample.transport_us)
        .sum::<u128>()
        / count;
    let render_us = timings.iter().map(|sample| sample.render_us).sum::<u128>() / count;
    let e2e_us = timings.iter().map(|sample| sample.e2e_us).sum::<u128>() / count;
    // 这些数来自 displayed ACK 的单调时钟。Wi-Fi 30 FPS 不是承诺。
    println!(
        "benchmark frames_sent={sent} frames_displayed={} dropped_frames={} transport_us={transport_us} render_us={render_us} e2e_us={e2e_us} device_frames={} acks={}",
        timings.len(),
        session.dropped_frames(),
        metrics.frames_received,
        metrics.acks_sent
    );
    let _ = acked;
    Ok(())
}

fn cmd_discover(opts: &GlobalOpts) -> Result<(), Box<dyn std::error::Error>> {
    #[derive(Serialize)]
    struct Row {
        device_id: Option<String>,
        endpoints: Vec<String>,
    }
    let mut combined = CombinedDiscovery::sim_defaults();
    if opts.sim {
        combined.push_provider(Box::new(MockMdnsProvider::with_defaults()));
    }
    // N43: best-effort live mDNS query alongside the serial scan. A sandbox with
    // no multicast route simply yields no candidates.
    let mut mdns = mdc_discovery::MdnsDiscovery::default();
    let _ = mdns.query_live(std::time::Duration::from_millis(150));
    combined.push_provider(Box::new(mdns));
    let rows: Vec<Row> = combined
        .discover()
        .into_iter()
        .map(|c| Row {
            device_id: c.device_id,
            endpoints: c.endpoints.iter().map(|e| format!("{e:?}")).collect(),
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&rows)?);
    Ok(())
}

fn cmd_flash_sim(opts: &GlobalOpts, image: &str) -> Result<(), Box<dyn std::error::Error>> {
    let path = Path::new(image);
    let bytes = std::fs::read(path)?;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let board = opts.board.clone();
    let _flags = flags_for_board(&board).ok_or("unknown board for flash flags")?;
    let plan = InstallPlan {
        port: opts.port.clone().unwrap_or_else(|| "sim:///flash".into()),
        artifact: Artifact {
            board_id: board.clone(),
            mcu: "esp32-s3".into(),
            runtime: "0.1.0".into(),
            protocol_major: VERSION.major,
            protocol_minor: VERSION.minor,
            image_size: bytes.len() as u64,
            sha256: hash,
            source: "cli-flash-sim".into(),
            public_key: Vec::new(),
            signature: Vec::new(),
        },
        expected_device_id: format!("sim-{board}"),
    };
    if plan.port.starts_with("sim://") {
        let mut flasher = SimFlasher::new(plan.expected_device_id.clone());
        let mut installer = Installer::default();
        let mut leases = PortLeases::default();
        installer.run(&mut leases, &plan, &mut flasher)?;
        println!(
            "flash-sim ok board={board} bytes={} reset={}",
            flasher.written.len(),
            flasher.reset_count
        );
        return Ok(());
    }
    // Dry-run EspTool argv for a real port (does not invoke esptool unless MDC_ESPTOOL_LIVE=1).
    let mut flasher = EspToolFlasher::new(path);
    if let Some(flags) = flags_for_board(&board) {
        flasher.flags = flags;
    }
    flasher.dry_run = std::env::var("MDC_ESPTOOL_LIVE").is_err();
    flasher.identity_override = Some(plan.expected_device_id.clone());
    let mut installer = Installer::default();
    let mut leases = PortLeases::default();
    installer.run(&mut leases, &plan, &mut flasher)?;
    println!("esptool argv: {}", flasher.last_argv.join(" "));
    println!("flash plan ok (dry_run={})", flasher.dry_run);
    Ok(())
}

fn pairing_path() -> std::path::PathBuf {
    // MDC_PAIRING_STORE 让每次验收使用自己的文件，避免并行测试覆盖同一份凭证。
    match std::env::var("MDC_PAIRING_STORE") {
        Ok(path) if !path.is_empty() => std::path::PathBuf::from(path),
        _ => std::env::temp_dir().join("mdc-pairing-store.json"),
    }
}

fn load_pairing_store() -> PairingStore {
    PairingStore::load_json(&pairing_path()).unwrap_or_default()
}

/// Reload pairing immediately before a TCP frame so a mid-session revoke is visible.
fn authorize_tcp_display(
    store_path: &Path,
    device_id: &str,
    address: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    PairingStore::load_json(store_path)
        .unwrap_or_default()
        .authorize_display_control(
            device_id,
            &Endpoint::Tcp {
                address: address.to_string(),
            },
        )
        .map_err(|e| -> Box<dyn std::error::Error> {
            format!("{e}; run `mdc pair {device_id} <token>` before network display control").into()
        })
}

fn send_authorized_tcp_frame(
    session: &mut Session<TcpTransport>,
    store_path: &Path,
    address: &str,
    frame: Frame,
) -> Result<u32, Box<dyn std::error::Error>> {
    let device_id = session
        .device
        .as_ref()
        .map(|device| device.capabilities.device_id.clone())
        .unwrap_or_default();
    authorize_tcp_display(store_path, &device_id, address)?;
    Ok(session.send_frame(frame)?)
}

/// After handshake, refuse Frame/Tile/OTA on network endpoints without pairing (H01).
fn gate_network_display(
    device_id: &str,
    endpoint: &Endpoint,
) -> Result<(), Box<dyn std::error::Error>> {
    load_pairing_store()
        .authorize_display_control(device_id, endpoint)
        .map_err(|e| -> Box<dyn std::error::Error> {
            format!("{e}; run `mdc pair {device_id} <token>` before network display control").into()
        })
}

fn cmd_pair(device_id: &str, token: &str) -> Result<(), Box<dyn std::error::Error>> {
    let path = pairing_path();
    let mut store = load_pairing_store();
    store.pair(device_id, token)?;
    store.save_json(&path)?;
    let store_text = path.display().to_string();
    println!(
        "{}",
        format_provision_log(&[
            ("device_id", device_id),
            ("token", token),
            ("store", store_text.as_str()),
        ])
    );
    Ok(())
}

fn cmd_unpair(device_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let path = pairing_path();
    let mut store = load_pairing_store();
    if store.revoke(device_id) {
        store.save_json(&path)?;
        println!("revoked pairing for {device_id}");
    } else {
        println!("no pairing for {device_id}");
    }
    Ok(())
}

fn cmd_authorize(
    device_id: &str,
    kind: &str,
    value: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let store = load_pairing_store();
    let endpoint = match kind {
        "ws" | "websocket" => Endpoint::WebSocket {
            address: value.into(),
        },
        "tcp" => Endpoint::Tcp {
            address: value.into(),
        },
        "serial" | "port" => Endpoint::Serial { port: value.into() },
        "memory" => Endpoint::Memory {
            label: value.into(),
        },
        other => return Err(format!("unknown endpoint kind {other}").into()),
    };
    match store.authorize_display_control(device_id, &endpoint) {
        Ok(()) => println!("authorized"),
        Err(e) => {
            eprintln!("denied: {e}");
            std::process::exit(1);
        }
    }
    Ok(())
}

fn cmd_switch_demo(opts: &GlobalOpts) -> Result<(), Box<dyn std::error::Error>> {
    use mdc_core::Tile;
    println!("switch-demo: cancel old Session, renegotiate, full frame before tile");
    let (host_a, mut peer_a) = FakeDevice::pair(BoardProfile::from_board_id(&opts.board));
    let mut session = Session::new(host_a);
    session.connect()?;
    peer_a.poll()?;
    session.poll()?;
    let (host_b, mut peer_b) = FakeDevice::pair(BoardProfile::from_board_id(&opts.board));
    session.switch_transport(host_b)?;
    peer_b.poll()?;
    session.poll()?;
    assert!(session.needs_full_frame);
    let w = peer_b.profile().width;
    let h = peer_b.profile().height;
    let tile_err = session.send_tile(Tile {
        surface_id: "main".into(),
        base_frame_id: 1,
        x: 0,
        y: 0,
        width: 1,
        height: 1,
        bytes: vec![0, 0],
    });
    assert!(
        tile_err.is_err(),
        "tile must be blocked until full frame ACK"
    );
    session.send_frame(Frame {
        surface_id: "main".into(),
        width: w,
        height: h,
        bytes: vec![0; usize::from(w) * usize::from(h) * 2],
    })?;
    peer_b.poll()?;
    session.poll()?;
    assert!(
        !session.needs_full_frame,
        "displayed ACK should clear needs_full_frame"
    );
    println!("switch-demo ok");
    Ok(())
}

fn cmd_h06_demo(opts: &GlobalOpts) -> Result<(), Box<dyn std::error::Error>> {
    println!("H06 sim demo: install → discover → display → input → OTA");
    let board = opts.board.clone();
    // 1) install (SimFlasher)
    let image = std::env::temp_dir().join("mdc-h06-merged.bin");
    std::fs::write(&image, b"MERGED-H06")?;
    cmd_flash_sim(opts, image.to_str().unwrap_or(""))?;
    println!("[ok] install");

    // 2) discover
    cmd_discover(opts)?;
    println!("[ok] discover");

    // 3) display + 4) input + 5) OTA on FakeDevice
    let (mut session, mut device, _) = open_sim(&board)?;
    let w = device.profile().width;
    let h = device.profile().height;
    session.send_frame(Frame {
        surface_id: "main".into(),
        width: w,
        height: h,
        bytes: vec![0xAB; usize::from(w) * usize::from(h) * 2],
    })?;
    pump_sim(&mut session, &mut device)?;
    println!("[ok] display frame");

    device.emit_input(InputEvent {
        surface_id: "main".into(),
        pointer_id: 1,
        phase: "down".into(),
        x: 10,
        y: 10,
    })?;
    session.poll()?;
    println!("[ok] input");

    if device.profile().ota {
        session.send_ota(OtaCommand {
            action: "begin".into(),
            version: Some("0.1.0-sim".into()),
            size: Some(6),
            sha256: Some("0".repeat(64)),
        })?;
        pump_sim(&mut session, &mut device)?;
        println!("[ok] ota");
    } else {
        println!("[skip] ota (board capability false)");
    }
    println!("H06 sim demo complete board={board}");
    Ok(())
}

#[derive(Serialize)]
struct AcceptReport {
    ok: bool,
    peer: String,
    address: String,
    device_id: String,
    handshake: bool,
    frame_acked: bool,
    tile_acked: bool,
    ota_failed: bool,
    firmware_before: String,
    firmware_after: String,
    image_bytes_included: bool,
    inspect_bytes: usize,
}

#[derive(Clone, Copy)]
struct InProcessPeer {
    sessions: usize,
    emit_input: bool,
}

enum PeerLaunch {
    /// `target/debug/mdc-sim-peer` or `MDC_SIM_PEER`.
    Binary(PathBuf),
    /// Same FakeDevice TCP loop, used when the peer binary is not built yet.
    InProcess(InProcessPeer),
}

#[derive(Clone, Copy)]
enum OtaGoal {
    /// Bad chunk rolls back; installed firmware stays put.
    Rollback,
    /// begin + good chunk + confirm installs the pending version.
    Install,
}

struct PeerStop {
    child: Option<Child>,
    thread: Option<thread::JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

impl PeerStop {
    fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for PeerStop {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn locate_sim_peer() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("MDC_SIM_PEER") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let name = if cfg!(windows) {
        "mdc-sim-peer.exe"
    } else {
        "mdc-sim-peer"
    };
    let mut dir = exe.parent().map(Path::to_path_buf)?;
    for _ in 0..3 {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// FakeDevice TCP loop shared with `tools/mdc-sim-peer`.
/// `sessions` is how many connections to serve before the thread exits.
fn serve_peer(
    listener: TcpListener,
    board: String,
    sessions: usize,
    emit_input: bool,
    stop: Arc<AtomicBool>,
) {
    let _ = listener.set_nonblocking(true);
    let profile = BoardProfile::from_board_id(&board);
    let mut accepted = 0usize;
    while accepted < sessions.max(1) && !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                accepted += 1;
                let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
                let _ = stream.set_write_timeout(Some(Duration::from_millis(50)));
                let Ok(transport) = TcpTransport::from_stream(stream) else {
                    continue;
                };
                let mut device = FakeDevice::from_transport(Box::new(transport), profile.clone());
                if emit_input {
                    device.emit_input_after_next_frame(mdc_protocol::InputEvent {
                        surface_id: "main".into(),
                        pointer_id: 1,
                        phase: "down".into(),
                        x: 6,
                        y: 7,
                    });
                }
                while !stop.load(Ordering::Relaxed) {
                    match device.poll() {
                        Ok(()) => thread::sleep(Duration::from_millis(5)),
                        Err(_) => break,
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break,
        }
    }
}

fn start_peer(
    board: &str,
    launch: PeerLaunch,
) -> Result<(String, String, PeerStop), Box<dyn std::error::Error>> {
    match launch {
        PeerLaunch::InProcess(options) => {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let address = listener.local_addr()?.to_string();
            let board = board.to_string();
            let stop = Arc::new(AtomicBool::new(false));
            let stop_flag = stop.clone();
            let thread = thread::spawn(move || {
                serve_peer(
                    listener,
                    board,
                    options.sessions,
                    options.emit_input,
                    stop_flag,
                );
            });
            Ok((
                address,
                "in-process".into(),
                PeerStop {
                    child: None,
                    thread: Some(thread),
                    stop,
                },
            ))
        }
        PeerLaunch::Binary(path) => {
            let probe = TcpListener::bind("127.0.0.1:0")?;
            let address = probe.local_addr()?.to_string();
            drop(probe);
            let child = Command::new(&path)
                .arg(&address)
                .arg(board)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("spawn {}: {e}", path.display()))?;
            Ok((
                address,
                "mdc-sim-peer".into(),
                PeerStop {
                    child: Some(child),
                    thread: None,
                    stop: Arc::new(AtomicBool::new(false)),
                },
            ))
        }
    }
}

fn connect_tcp(address: &str) -> Result<TcpTransport, Box<dyn std::error::Error>> {
    let mut last = String::from("not attempted");
    for _ in 0..40 {
        match TcpTransport::connect(address, Duration::from_millis(200)) {
            Ok(transport) => return Ok(transport),
            Err(error) => {
                last = error.to_string();
                thread::sleep(Duration::from_millis(25));
            }
        }
    }
    Err(format!("tcp connect {address}: {last}").into())
}

fn pump_until_idle(session: &mut Session<TcpTransport>) -> Result<(), Box<dyn std::error::Error>> {
    for _ in 0..80 {
        session.poll()?;
        if session.state == ConnectionState::Ready && session.pending_request_count() == 0 {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err(format!(
        "timed out waiting for ack (state={:?} pending={})",
        session.state,
        session.pending_request_count()
    )
    .into())
}

fn pump_until_protocol_error(
    session: &mut Session<TcpTransport>,
) -> Result<String, Box<dyn std::error::Error>> {
    for _ in 0..80 {
        match session.poll() {
            Err(error) => return Ok(error.to_string()),
            Ok(_) if session.pending_request_count() == 0 => {
                return Err("ota completed without an error".into());
            }
            Ok(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
    Err("timed out waiting for ota error".into())
}

fn isolated_pairing_path() -> PathBuf {
    if let Ok(path) = std::env::var("MDC_PAIRING_STORE") {
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    static SEQ: AtomicU64 = AtomicU64::new(1);
    std::env::temp_dir().join(format!(
        "mdc-pairing-{}-{}.json",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

fn ensure_paired_at(
    path: &Path,
    device_id: &str,
    token: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut store = PairingStore::load_json(path).unwrap_or_default();
    if !store.is_paired(device_id) {
        store.pair(device_id, token)?;
        store.save_json(path)?;
    }
    Ok(())
}

fn authorize_at(
    path: &Path,
    device_id: &str,
    endpoint: &Endpoint,
) -> Result<(), Box<dyn std::error::Error>> {
    PairingStore::load_json(path)
        .unwrap_or_default()
        .authorize_display_control(device_id, endpoint)
        .map_err(|error| -> Box<dyn std::error::Error> { error.to_string().into() })
}

fn run_sim_accept(
    board: &str,
    launch: PeerLaunch,
    goal: OtaGoal,
) -> Result<AcceptReport, Box<dyn std::error::Error>> {
    let (address, peer_name, peer) = start_peer(board, launch)?;
    let store = isolated_pairing_path();
    let result = accept_on(board, &address, &peer_name, &store, goal);
    let _ = std::fs::remove_file(&store);
    // Session is already dropped, so the peer sees the socket close before join.
    peer.stop();
    result
}

fn accept_on(
    board: &str,
    address: &str,
    peer_name: &str,
    store: &Path,
    goal: OtaGoal,
) -> Result<AcceptReport, Box<dyn std::error::Error>> {
    let transport = connect_tcp(address)?;
    let manager = Arc::new(Mutex::new(DeviceManager::new()));
    let mut session = Session::new(transport);
    session.set_manager(manager.clone());
    session.connect()?;
    pump_until_idle(&mut session)?;
    let (device_id, firmware_before, width, height, ota) = {
        let device = session
            .device
            .as_ref()
            .ok_or("handshake produced no device")?;
        let surface = device
            .capabilities
            .surfaces
            .first()
            .ok_or("device has no surface")?;
        (
            device.capabilities.device_id.clone(),
            device.capabilities.firmware.clone(),
            surface.width,
            surface.height,
            device.capabilities.ota,
        )
    };
    if !ota {
        return Err(
            format!("sim-accept needs an OTA-capable board; {board} reported ota=false").into(),
        );
    }
    ensure_paired_at(store, &device_id, "sim-accept")?;
    authorize_at(
        store,
        &device_id,
        &Endpoint::Tcp {
            address: address.to_string(),
        },
    )?;
    let pixel_len = usize::from(width) * usize::from(height) * 2;
    let mut bytes = vec![0u8; pixel_len];
    if bytes.len() >= 2 {
        bytes[0] = 0x00;
        bytes[1] = 0xf8;
    }
    session.send_frame(Frame {
        surface_id: "main".into(),
        width,
        height,
        bytes,
    })?;
    pump_until_idle(&mut session)?;
    session.send_tile(mdc_core::Tile {
        surface_id: "main".into(),
        base_frame_id: 1,
        x: 0,
        y: 0,
        width: 1,
        height: 1,
        bytes: vec![0x00, 0xf8],
    })?;
    pump_until_idle(&mut session)?;
    let inspect = {
        let guard = manager.lock().map_err(|e| e.to_string())?;
        inspect_json(&guard, None)
    };
    if inspect.contains("\"pixels\"") || !inspect.contains("\"image_bytes_included\": false") {
        return Err("inspect snapshot included pixel buffers".into());
    }
    session.send_ota(OtaCommand {
        action: "begin".into(),
        version: Some("2.0.0".into()),
        size: Some(64),
        sha256: Some("0".repeat(64)),
    })?;
    pump_until_idle(&mut session)?;
    let (ota_failed, firmware_after) = match goal {
        OtaGoal::Rollback => {
            session.send_ota(OtaCommand {
                action: "chunk".into(),
                version: None,
                size: None,
                sha256: Some("bad".into()),
            })?;
            let ota_error = pump_until_protocol_error(&mut session)?;
            if !ota_error.contains("E_OTA") {
                return Err(format!("unexpected ota failure: {ota_error}").into());
            }
            session.refresh_capabilities()?;
            pump_until_idle(&mut session)?;
            let firmware_after = session
                .device
                .as_ref()
                .map(|d| d.capabilities.firmware.clone())
                .unwrap_or_default();
            if firmware_after != firmware_before {
                return Err(
                    format!("firmware changed from {firmware_before} to {firmware_after}").into(),
                );
            }
            (true, firmware_after)
        }
        OtaGoal::Install => {
            session.send_ota(OtaCommand {
                action: "chunk".into(),
                version: None,
                size: None,
                sha256: Some("a".repeat(64)),
            })?;
            pump_until_idle(&mut session)?;
            session.send_ota(OtaCommand {
                action: "confirm".into(),
                version: Some("2.0.0".into()),
                size: None,
                sha256: None,
            })?;
            pump_until_idle(&mut session)?;
            session.refresh_capabilities()?;
            pump_until_idle(&mut session)?;
            let firmware_after = session
                .device
                .as_ref()
                .map(|d| d.capabilities.firmware.clone())
                .unwrap_or_default();
            if firmware_after != "2.0.0" {
                return Err(format!("confirm left firmware at {firmware_after}").into());
            }
            (false, firmware_after)
        }
    };
    session.disconnect();
    Ok(AcceptReport {
        ok: true,
        peer: peer_name.to_string(),
        address: address.to_string(),
        device_id,
        handshake: true,
        frame_acked: true,
        tile_acked: true,
        ota_failed,
        firmware_before,
        firmware_after,
        image_bytes_included: false,
        inspect_bytes: inspect.len(),
    })
}

fn peer_launch_for_cli() -> PeerLaunch {
    match locate_sim_peer() {
        Some(path) => PeerLaunch::Binary(path),
        None => PeerLaunch::InProcess(InProcessPeer {
            sessions: 1,
            emit_input: false,
        }),
    }
}

fn cmd_sim_accept(opts: &GlobalOpts) -> Result<(), Box<dyn std::error::Error>> {
    let report = run_sim_accept(&opts.board, peer_launch_for_cli(), OtaGoal::Rollback)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn cmd_ota_confirm(opts: &GlobalOpts) -> Result<(), Box<dyn std::error::Error>> {
    let report = run_sim_accept(&opts.board, peer_launch_for_cli(), OtaGoal::Install)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
fn run_tcp_reconnect(board: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (address, _, peer) = start_peer(
        board,
        PeerLaunch::InProcess(InProcessPeer {
            sessions: 2,
            emit_input: false,
        }),
    )?;
    let store = isolated_pairing_path();
    let result = (|| {
        let transport = connect_tcp(&address)?;
        let mut session = Session::new(transport);
        session.connect()?;
        pump_until_idle(&mut session)?;
        let device_id = session
            .device
            .as_ref()
            .map(|device| device.capabilities.device_id.clone())
            .ok_or("handshake produced no device")?;
        let (width, height) = session
            .device
            .as_ref()
            .and_then(|device| device.capabilities.surfaces.first())
            .map(|surface| (surface.width, surface.height))
            .ok_or("device has no surface")?;
        ensure_paired_at(&store, &device_id, "reconnect")?;
        let endpoint = Endpoint::Tcp {
            address: address.clone(),
        };
        authorize_at(&store, &device_id, &endpoint)?;
        let pixels = usize::from(width) * usize::from(height) * 2;
        session.send_frame(Frame {
            surface_id: "main".into(),
            width,
            height,
            bytes: vec![0x11; pixels],
        })?;
        pump_until_idle(&mut session)?;
        session.disconnect();
        let next = connect_tcp(&address)?;
        session.switch_transport(next)?;
        pump_until_idle(&mut session)?;
        if !session.needs_full_frame {
            return Err("reconnect must require a full frame".into());
        }
        let blocked = session.send_tile(mdc_core::Tile {
            surface_id: "main".into(),
            base_frame_id: 1,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            bytes: vec![0, 0],
        });
        if blocked.is_ok() {
            return Err("tile was accepted before a full frame".into());
        }
        session.send_frame(Frame {
            surface_id: "main".into(),
            width,
            height,
            bytes: vec![0x22; pixels],
        })?;
        pump_until_idle(&mut session)?;
        if session.needs_full_frame {
            return Err("displayed frame did not clear the full-frame gate".into());
        }
        session.send_tile(mdc_core::Tile {
            surface_id: "main".into(),
            base_frame_id: 1,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            bytes: vec![0x22, 0x22],
        })?;
        pump_until_idle(&mut session)?;
        session.disconnect();
        Ok(())
    })();
    let _ = std::fs::remove_file(&store);
    peer.stop();
    result
}

#[cfg(test)]
fn run_tcp_input(board: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (address, _, peer) = start_peer(
        board,
        PeerLaunch::InProcess(InProcessPeer {
            sessions: 1,
            emit_input: true,
        }),
    )?;
    let store = isolated_pairing_path();
    let result = (|| {
        let transport = connect_tcp(&address)?;
        let manager = Arc::new(Mutex::new(DeviceManager::new()));
        let mut session = Session::new(transport);
        session.set_manager(manager.clone());
        session.connect()?;
        pump_until_idle(&mut session)?;
        let device_id = session
            .device
            .as_ref()
            .map(|device| device.capabilities.device_id.clone())
            .ok_or("handshake produced no device")?;
        let (width, height) = session
            .device
            .as_ref()
            .and_then(|device| device.capabilities.surfaces.first())
            .map(|surface| (surface.width, surface.height))
            .ok_or("device has no surface")?;
        ensure_paired_at(&store, &device_id, "input")?;
        authorize_at(
            &store,
            &device_id,
            &Endpoint::Tcp {
                address: address.clone(),
            },
        )?;
        let pixels = usize::from(width) * usize::from(height) * 2;
        session.send_frame(Frame {
            surface_id: "main".into(),
            width,
            height,
            bytes: vec![0x33; pixels],
        })?;
        pump_until_idle(&mut session)?;
        for _ in 0..30 {
            let _ = session.poll();
            let events: Vec<_> = manager
                .lock()
                .map_err(|error| error.to_string())?
                .drain_events()
                .collect();
            if events.iter().any(|event| {
                matches!(
                    event,
                    mdc_core::ManagerEvent::Input { event, .. }
                        if event.phase == "down" && event.x == 6 && event.y == 7
                )
            }) {
                session.disconnect();
                return Ok(());
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err("tcp peer did not deliver the scripted touch".into())
    })();
    let _ = std::fs::remove_file(&store);
    peer.stop();
    result
}

fn one_pixel_tile() -> Tile {
    Tile {
        surface_id: "main".into(),
        base_frame_id: 1,
        x: 0,
        y: 0,
        width: 1,
        height: 1,
        bytes: vec![0x44, 0x44],
    }
}

/// linux-virt：握手必须是 800×480。全帧显示之前的 Tile 被设备拒绝，全帧 ACK 之后同一块 Tile 成功。
fn linux_accept_memory() -> Result<(), Box<dyn std::error::Error>> {
    let (mut session, mut device, _) = open_sim("linux-virt")?;
    let (device_id, width, height) = linux_surface(&session)?;
    if device_id != "sim-linux-virt" || width != 800 || height != 480 {
        return Err(format!("linux-virt handshake {device_id} {width}x{height}").into());
    }
    session.send_tile(one_pixel_tile())?;
    device.poll()?;
    match session.poll() {
        Err(error) if error.to_string().contains("E_BASE") => {}
        other => {
            return Err(format!("tile before a full frame should be E_BASE, got {other:?}").into())
        }
    }
    session.send_frame(Frame {
        surface_id: "main".into(),
        width,
        height,
        bytes: vec![0x44; usize::from(width) * usize::from(height) * 2],
    })?;
    pump_sim(&mut session, &mut device)?;
    session.send_tile(one_pixel_tile())?;
    pump_sim(&mut session, &mut device)?;
    println!("linux-accept ok {width}x{height}");
    Ok(())
}

fn linux_surface<T: mdc_transport::Transport>(
    session: &Session<T>,
) -> Result<(String, u16, u16), Box<dyn std::error::Error>> {
    let device = session
        .device
        .as_ref()
        .ok_or("handshake produced no device")?;
    let surface = device
        .capabilities
        .surfaces
        .first()
        .ok_or("linux-virt advertised no surface")?;
    Ok((
        device.capabilities.device_id.clone(),
        surface.width,
        surface.height,
    ))
}

fn linux_accept_tcp(address: &str) -> Result<(), Box<dyn std::error::Error>> {
    let transport = connect_tcp(address)?;
    let mut session = Session::new(transport);
    session.connect()?;
    pump_until_idle(&mut session)?;
    let (device_id, width, height) = linux_surface(&session)?;
    if width != 800 || height != 480 {
        return Err(format!("tcp linux peer {device_id} is {width}x{height}").into());
    }
    gate_network_display(
        &device_id,
        &Endpoint::Tcp {
            address: address.into(),
        },
    )?;
    session.send_tile(one_pixel_tile())?;
    let mut rejected = false;
    for _ in 0..40 {
        match session.poll() {
            Err(error) if error.to_string().contains("E_BASE") => {
                rejected = true;
                break;
            }
            Err(error) => return Err(error.to_string().into()),
            Ok(_) => thread::sleep(Duration::from_millis(15)),
        }
    }
    if !rejected {
        return Err("tile before a full frame was not rejected".into());
    }
    session.send_frame(Frame {
        surface_id: "main".into(),
        width,
        height,
        bytes: vec![0x44; usize::from(width) * usize::from(height) * 2],
    })?;
    pump_until_idle(&mut session)?;
    session.send_tile(one_pixel_tile())?;
    pump_until_idle(&mut session)?;
    println!("linux-accept ok {width}x{height} peer={device_id}");
    Ok(())
}

fn cmd_linux_accept(opts: &GlobalOpts) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(address) = &opts.address {
        linux_accept_tcp(address)
    } else {
        linux_accept_memory()
    }
}

fn cmd_preview_mesh() -> Result<(), Box<dyn std::error::Error>> {
    let points = [(1i16, 1), (10, 1), (1, 10)];
    let pixels = mdc_bake::raster_triangle_fan(&points, 16, 16, 0xF800)?;
    let covered = pixels.chunks(2).filter(|px| px != &[0, 0]).count();
    println!("preview-mesh points={} covered={covered}", points.len());
    if covered == 0 {
        return Err("preview mesh covered no pixels".into());
    }
    Ok(())
}

fn cmd_raster_demo() -> Result<(), Box<dyn std::error::Error>> {
    let pixels = mdc_bake::raster_triangle_fan(&[(2, 2), (20, 2), (2, 20)], 32, 32, 0xF800)?;
    let covered = pixels.chunks(2).filter(|px| px != &[0, 0]).count();
    println!("raster-demo covered={covered} bytes={}", pixels.len());
    if covered == 0 {
        return Err("triangle fan covered no pixels".into());
    }
    Ok(())
}

fn usage() {
    println!(
        "mdc [--sim] [--board id] [--address host:port] [--port /dev/cu.usbmodem*] \
         devices|inspect|send-image <path>|benchmark [N]|simulate|discover|\
flash-sim <merged.bin>|h06-demo|sim-accept|ota-confirm|linux-accept|pair <id> <token>|unpair <id>|\
authorize <id> <ws|tcp|serial|memory> <ep>|switch-demo|mirror [--frames N] [--write-dir DIR] [--format ppm|bmp] [--serve ADDR] [--http ADDR]|\
package <model.gltf> [out.mdcpkg]|install-package <file.mdcpkg>|\
config get <key>|config set <key> <value>|bake <image>... [--frames N] [--sprite N] [--out DIR] [--package OUT.mdcpkg]|\
bake-mesh <model.gltf> [--frames N] [--sprite N] [--animation N] [--texture tex.png] [--out DIR] [--package OUT.mdcpkg]|\
render-mesh <model.gltf> [--texture tex.png] [--animation N] [--time T] [--sprite N] --out f.ppm|\
render-package <file.mdcpkg> [--frame N] --out f.ppm|\
scene <file.json>|scene clear|scene-render <file.json> --out f.ppm|raster-demo|preview-mesh|inspect [--watch --count N --interval MS]"
    );
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let (opts, args) = parse_globals(&raw);
    let result = match args.first().map(String::as_str) {
        Some("simulate") => cmd_simulate(&opts),
        Some("devices") => {
            let mut o = opts.clone();
            o.sim = true; // devices without live peer uses FakeDevice when --sim or by default here
            if opts.sim || opts.address.is_none() {
                o.sim = true;
            }
            cmd_devices(&o)
        }
        Some("inspect") => {
            let mut o = opts.clone();
            if opts.sim || opts.address.is_none() {
                o.sim = true;
            }
            cmd_inspect(&o, &args[1..])
        }
        Some("send-image") => {
            let path = args.get(1).cloned().unwrap_or_default();
            if path.is_empty() {
                eprintln!("send-image requires a path");
                std::process::exit(2);
            }
            let mut o = opts.clone();
            if o.address.is_none() {
                o.sim = true;
            }
            cmd_send_image(&o, &path)
        }
        Some("benchmark") => {
            let n = args
                .get(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_BENCHMARK_FRAMES);
            let mut o = opts.clone();
            o.sim = true;
            cmd_benchmark(&o, n)
        }
        Some("discover") => {
            let mut o = opts.clone();
            if o.address.is_none() && o.port.is_none() {
                o.sim = true;
            }
            cmd_discover(&o)
        }
        Some("flash-sim") => {
            let image = args.get(1).cloned().unwrap_or_default();
            if image.is_empty() {
                eprintln!("flash-sim requires a merged.bin path");
                std::process::exit(2);
            }
            cmd_flash_sim(&opts, &image)
        }
        Some("h06-demo") => {
            let mut o = opts.clone();
            o.sim = true;
            cmd_h06_demo(&o)
        }
        Some("sim-accept") => cmd_sim_accept(&opts),
        Some("mirror") => {
            let mut o = opts.clone();
            o.sim = true;
            cmd_mirror(&o, &args[1..])
        }
        Some("package") => {
            let path = args.get(1).cloned().unwrap_or_default();
            if path.is_empty() {
                eprintln!("package requires a model path");
                std::process::exit(2);
            }
            cmd_package(&opts, &path, args.get(2).cloned())
        }
        Some("install-package") => {
            let path = args.get(1).cloned().unwrap_or_default();
            if path.is_empty() {
                eprintln!("install-package requires a package path");
                std::process::exit(2);
            }
            let mut o = opts.clone();
            o.sim = true;
            cmd_install_package(&o, &path)
        }
        Some("config") => {
            let action = args.get(1).cloned().unwrap_or_default();
            let key = args.get(2).cloned().unwrap_or_default();
            if action.is_empty() || key.is_empty() {
                eprintln!("config get <key> | config set <key> <value>");
                std::process::exit(2);
            }
            let mut o = opts.clone();
            o.sim = true;
            cmd_config(&o, &action, &key, args.get(3).map(String::as_str))
        }
        Some("bake") => cmd_bake(&args[1..]),
        Some("scene") => {
            let mut o = opts.clone();
            o.sim = true;
            cmd_scene(&o, &args[1..])
        }
        Some("scene-render") => {
            let mut o = opts.clone();
            o.sim = true;
            cmd_scene_render(&o, &args[1..])
        }
        Some("ota-confirm") => cmd_ota_confirm(&opts),
        Some("linux-accept") => cmd_linux_accept(&opts),
        Some("pair") => {
            let id = args.get(1).cloned().unwrap_or_default();
            let token = args.get(2).cloned().unwrap_or_default();
            if id.is_empty() || token.is_empty() {
                eprintln!("pair <device_id> <token>");
                std::process::exit(2);
            }
            cmd_pair(&id, &token)
        }
        Some("unpair") => {
            let id = args.get(1).cloned().unwrap_or_default();
            if id.is_empty() {
                eprintln!("unpair <device_id>");
                std::process::exit(2);
            }
            cmd_unpair(&id)
        }
        Some("authorize") => {
            let id = args.get(1).cloned().unwrap_or_default();
            let kind = args.get(2).cloned().unwrap_or_default();
            let value = args.get(3).cloned().unwrap_or_default();
            if id.is_empty() || kind.is_empty() || value.is_empty() {
                eprintln!("authorize <device_id> <ws|tcp|serial|memory> <endpoint>");
                std::process::exit(2);
            }
            cmd_authorize(&id, &kind, &value)
        }
        Some("raster-demo") => cmd_raster_demo(),
        Some("preview-mesh") => cmd_preview_mesh(),
        Some("bake-mesh") => cmd_bake_mesh(&args[1..]),
        Some("render-mesh") => cmd_render_mesh(&args[1..]),
        Some("render-package") => cmd_render_package(&args[1..]),
        Some("switch-demo") => {
            let mut o = opts.clone();
            o.sim = true;
            cmd_switch_demo(&o)
        }
        Some("help") | Some("-h") | Some("--help") | None => {
            usage();
            Ok(())
        }
        Some(other) => {
            eprintln!("unknown command: {other}");
            usage();
            std::process::exit(2);
        }
    };
    if let Err(error) = result {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rgb565_is_little_endian_and_bounded() {
        let path = std::env::temp_dir().join(format!("mdc-cli-test-{}.png", std::process::id()));
        let image = image::RgbImage::from_pixel(1, 1, image::Rgb([255, 0, 0]));
        image.save(&path).unwrap();
        assert_eq!(load_rgb565(&path, 1, 1).unwrap(), vec![0, 248]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn parse_globals_board_and_sim() {
        let (opts, rest) = parse_globals(&[
            "--sim".into(),
            "--board".into(),
            "linux-virt".into(),
            "simulate".into(),
        ]);
        assert!(opts.sim);
        assert_eq!(opts.board, "linux-virt");
        assert_eq!(rest, vec!["simulate".to_string()]);
    }

    #[test]
    fn linux_accept_memory_requires_full_frame_before_tile() {
        linux_accept_memory().unwrap();
    }

    #[test]
    fn revoke_blocks_the_next_frame_on_the_same_tcp_session() {
        let store = std::env::temp_dir().join(format!(
            "mdc-pairing-revoke-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let mut pairing = PairingStore::default();
        pairing.pair("sim-dual", "token").unwrap();
        pairing.save_json(&store).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_server = stop.clone();
        let pixels = Arc::new(Mutex::new(Vec::new()));
        let pixels_server = pixels.clone();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
            let transport = TcpTransport::from_stream(stream).unwrap();
            let mut device = FakeDevice::from_transport(
                Box::new(transport),
                BoardProfile::from_board_id("dual-sim"),
            );
            while !stop_server.load(Ordering::Relaxed) {
                let _ = device.poll();
                *pixels_server.lock().unwrap() = device.pixels().to_vec();
                thread::sleep(Duration::from_millis(5));
            }
        });
        let transport = connect_tcp(&address).unwrap();
        let mut session = Session::new(transport);
        session.connect().unwrap();
        pump_until_idle(&mut session).unwrap();
        send_authorized_tcp_frame(
            &mut session,
            &store,
            &address,
            Frame {
                surface_id: "main".into(),
                width: 8,
                height: 4,
                bytes: vec![0x21; 64],
            },
        )
        .unwrap();
        pump_until_idle(&mut session).unwrap();
        let shown = pixels.lock().unwrap().clone();
        assert_eq!(shown, vec![0x21; 64]);
        let mut pairing = PairingStore::load_json(&store).unwrap();
        assert!(pairing.revoke("sim-dual"));
        pairing.save_json(&store).unwrap();
        let denied = send_authorized_tcp_frame(
            &mut session,
            &store,
            &address,
            Frame {
                surface_id: "main".into(),
                width: 8,
                height: 4,
                bytes: vec![0x44; 64],
            },
        );
        assert!(denied.is_err());
        thread::sleep(Duration::from_millis(40));
        assert_eq!(pixels.lock().unwrap().as_slice(), shown.as_slice());
        stop.store(true, Ordering::Relaxed);
        drop(session);
        let _ = server.join();
        let _ = std::fs::remove_file(store);
    }

    #[cfg(all(unix, feature = "serial"))]
    #[test]
    fn pty_hello_returns_capabilities_and_unknown_port_waits() {
        use mdc_protocol::{
            encode_control, Capabilities, Hello, MessageType, Packet, Surface, VERSION,
        };
        use mdc_transport::{
            is_candidate_serial_port, SerialOpenGuard, SerialTransport, Transport,
        };
        use std::io::{Read, Write};
        use std::os::unix::io::FromRawFd;

        assert!(!is_candidate_serial_port("/dev/cu.Bluetooth-Incoming-Port"));
        let mut guard = SerialOpenGuard::default();
        assert_eq!(guard.note_failure(true), 300_000);
        assert_eq!(guard.backoff_ms(), 300_000);

        #[link(name = "c")]
        extern "C" {
            fn openpty(
                amaster: *mut i32,
                aslave: *mut i32,
                name: *mut i8,
                termp: *mut u8,
                winp: *mut u8,
            ) -> i32;
            fn close(fd: i32) -> i32;
            fn fcntl(fd: i32, cmd: i32, arg: i32) -> i32;
        }
        let mut master_fd = 0i32;
        let mut slave_fd = 0i32;
        let mut name = [0i8; 128];
        let rc = unsafe {
            openpty(
                &mut master_fd,
                &mut slave_fd,
                name.as_mut_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0, "openpty");
        unsafe {
            let flags = fcntl(master_fd, 3, 0);
            fcntl(master_fd, 4, flags | 4);
        }
        let path = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        let opened = SerialTransport::open(&path, 115_200, Duration::from_millis(50));
        let Ok(mut serial) = opened else {
            eprintln!("pty serial open is unavailable on this host; guard backoff still holds");
            unsafe {
                close(master_fd);
                close(slave_fd);
            }
            return;
        };
        let hello = Packet {
            version: VERSION,
            kind: MessageType::Hello,
            flags: 0,
            request_id: 1,
            payload: encode_control(&Hello {
                major: 1,
                minor: 0,
                max_message: 4096,
                max_chunk: 1024,
            })
            .unwrap(),
        }
        .encode(4096)
        .unwrap();
        serial.write(&hello).unwrap();
        let mut master = unsafe { std::fs::File::from_raw_fd(master_fd) };
        let mut incoming = Vec::new();
        let mut saw_hello = false;
        for _ in 0..40 {
            let mut buf = [0u8; 512];
            match master.read(&mut buf) {
                Ok(0) | Err(_) => {}
                Ok(n) => incoming.extend_from_slice(&buf[..n]),
            }
            if let Ok((packet, _)) = Packet::decode(&incoming, 4096) {
                if packet.kind == MessageType::Hello {
                    saw_hello = true;
                    break;
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(saw_hello, "serial hello did not reach the pty master");
        let caps = Packet {
            version: VERSION,
            kind: MessageType::Capabilities,
            flags: 0,
            request_id: 1,
            payload: encode_control(&Capabilities {
                device_id: "pty".into(),
                firmware: "1.0.0".into(),
                surfaces: vec![Surface {
                    id: "main".into(),
                    width: 2,
                    height: 2,
                    pixel_format: "RGB565".into(),
                    stride: 4,
                    rotation: 0,
                }],
                frame: true,
                tile: true,
                touch: false,
                ota: false,
                max_message: 4096,
                max_chunk: 1024,
                max_in_flight: 1,
                max_fps: 30,
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
        master.write_all(&caps).unwrap();
        let mut reply = Vec::new();
        let mut saw_caps = false;
        for _ in 0..40 {
            match serial.read() {
                Ok(Some(bytes)) => reply.extend(bytes),
                Ok(None) => {}
                Err(error) => panic!("serial read: {error}"),
            }
            if let Ok((packet, _)) = Packet::decode(&reply, 4096) {
                if packet.kind == MessageType::Capabilities {
                    saw_caps = true;
                    break;
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(saw_caps, "pty did not return capabilities");
        drop(serial);
        drop(master);
        unsafe {
            close(slave_fd);
        }
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn websocket_without_pong_does_not_display_the_open_frame() {
        use mdc_core::{CoreError, SessionConfig, Tile};
        use mdc_protocol::{encode_control, Capabilities, MessageType, Packet, Surface, VERSION};
        use mdc_transport::{Transport, WebSocketTransport};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                let mut peer = WebSocketTransport::accept(stream).unwrap();
                let mut answered = false;
                for _ in 0..400 {
                    match peer.read() {
                        Ok(Some(bytes)) => {
                            if !answered {
                                if let Ok((packet, _)) = Packet::decode(&bytes, 1024 * 1024) {
                                    if packet.kind == MessageType::Hello {
                                        let caps = Capabilities {
                                            device_id: "ws-silent".into(),
                                            firmware: "1".into(),
                                            surfaces: vec![Surface {
                                                id: "main".into(),
                                                width: 2,
                                                height: 2,
                                                pixel_format: "RGB565".into(),
                                                stride: 4,
                                                rotation: 0,
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
                                        };
                                        let reply = Packet {
                                            version: VERSION,
                                            kind: MessageType::Capabilities,
                                            flags: 0,
                                            request_id: packet.request_id,
                                            payload: encode_control(&caps).unwrap(),
                                        };
                                        peer.write(&reply.encode(4096).unwrap()).unwrap();
                                        answered = true;
                                    }
                                }
                            }
                        }
                        Ok(None) => thread::sleep(Duration::from_millis(5)),
                        Err(_) => break,
                    }
                }
            }
        });
        let transport =
            WebSocketTransport::connect(&format!("ws://127.0.0.1:{port}/"), Duration::from_secs(2))
                .unwrap();
        let mut session = Session::with_config(
            transport,
            SessionConfig {
                connect_timeout_ms: 10_000,
                ack_timeout_ms: 0,
                heartbeat_ms: 1_000,
                reconnect: true,
            },
        );
        session.connect().unwrap();
        for _ in 0..40 {
            if session.poll().unwrap() == ConnectionState::Ready {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(session.state, ConnectionState::Ready);
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![9; 8],
            })
            .unwrap();
        assert_eq!(session.tick(1_000).unwrap(), ConnectionState::Ready);
        assert_eq!(session.tick(2_000).unwrap(), ConnectionState::Ready);
        assert_eq!(session.tick(3_000).unwrap(), ConnectionState::Ready);
        assert_eq!(session.tick(4_000), Err(CoreError::HeartbeatMissed));
        assert_eq!(session.state, ConnectionState::Reconnecting);
        assert!(session.pending_request_count() >= 1);
        assert_eq!(session.tick(5_000).unwrap(), ConnectionState::Connecting);
        let transport =
            WebSocketTransport::connect(&format!("ws://127.0.0.1:{port}/"), Duration::from_secs(2))
                .unwrap();
        session.provide_transport(transport).unwrap();
        for _ in 0..40 {
            if session.poll().unwrap() == ConnectionState::Ready {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
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
        drop(session);
        let _ = server.join();
    }

    fn bound_message(message: &str) -> String {
        let mut owned: String = message.chars().take(160).collect();
        if message.chars().count() > 160 {
            owned.push('…');
        }
        owned
    }

    #[test]
    fn sim_accept_failed_ota_keeps_firmware() {
        let report = run_sim_accept(
            "esp32-jc3248w535-sim",
            PeerLaunch::InProcess(InProcessPeer {
                sessions: 1,
                emit_input: false,
            }),
            OtaGoal::Rollback,
        )
        .unwrap();
        assert!(report.ok);
        assert_eq!(report.peer, "in-process");
        assert_eq!(report.firmware_before, "1.0.0");
        assert_eq!(report.firmware_after, "1.0.0");
        assert!(report.handshake && report.frame_acked && report.tile_acked);
        assert!(report.ota_failed);
        assert!(!report.image_bytes_included);
        assert!(report.inspect_bytes < 4096);
        assert_eq!(report.device_id, "sim-esp32-jc3248w535");
    }

    #[test]
    fn ota_confirm_installs_pending_firmware() {
        let report = run_sim_accept(
            "esp32-jc3248w535-sim",
            PeerLaunch::InProcess(InProcessPeer {
                sessions: 1,
                emit_input: false,
            }),
            OtaGoal::Install,
        )
        .unwrap();
        assert!(report.ok);
        assert!(!report.ota_failed);
        assert_eq!(report.firmware_before, "1.0.0");
        assert_eq!(report.firmware_after, "2.0.0");
    }

    #[test]
    fn tcp_reconnect_blocks_tile_until_full_frame() {
        run_tcp_reconnect("esp32-jc3248w535-sim").unwrap();
    }

    #[test]
    fn tcp_scripted_input_reaches_manager() {
        run_tcp_input("esp32-jc3248w535-sim").unwrap();
    }

    #[test]
    fn inspect_json_omits_pixel_buffers() {
        let json = inspect_json(&DeviceManager::new(), None);
        assert!(json.contains("\"image_bytes_included\": false"));
        assert!(!json.contains("\"pixels\""));
        assert!(json.len() < 4096);
        assert_eq!(bound_message(&"x".repeat(10_000)).chars().count(), 161);
    }

    #[test]
    fn inspect_reports_mirror_and_renderer_capability() {
        let (_session, _device, manager) = open_sim("linux-virt").unwrap();
        let json = inspect_json(&manager.lock().unwrap(), None);
        assert!(json.contains("\"renderer\""));
        assert!(json.contains("\"mesh2d\": true"));
        assert!(json.contains("\"mirror\""));
        assert!(json.contains("\"supported\": true"));
        assert!(json.contains("\"configKeys\""));
        assert!(json.contains("\"scene\""));
        assert!(!json.contains("\"pixels\""));
    }

    #[test]
    fn inspect_watch_streams_a_bounded_number_of_snapshots() {
        let opts = GlobalOpts {
            sim: true,
            address: None,
            port: None,
            board: "linux-virt".into(),
        };
        let args = vec![
            "--watch".to_string(),
            "--count".to_string(),
            "2".to_string(),
            "--interval".to_string(),
            "0".to_string(),
        ];
        cmd_inspect(&opts, &args).unwrap();
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn websocket_unpaired_refuses_display_then_frame_acks() {
        use mdc_transport::WebSocketTransport;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("ws accept");
            let socket = WebSocketTransport::accept(stream).expect("ws handshake");
            let mut device = FakeDevice::from_transport(Box::new(socket), BoardProfile::default());
            while let Ok(()) = device.poll() {
                thread::sleep(Duration::from_millis(5));
            }
        });
        let url = format!("ws://127.0.0.1:{port}/");
        let transport =
            WebSocketTransport::connect(&url, Duration::from_secs(2)).expect("ws connect");
        let mut session = Session::new(transport);
        session.connect().unwrap();
        for _ in 0..40 {
            if session.poll().is_ok()
                && session.state == ConnectionState::Ready
                && session.pending_request_count() == 0
            {
                break;
            }
            thread::sleep(Duration::from_millis(15));
        }
        assert_eq!(session.state, ConnectionState::Ready);
        let device_id = session
            .device
            .as_ref()
            .unwrap()
            .capabilities
            .device_id
            .clone();
        let endpoint = Endpoint::WebSocket {
            address: url.clone(),
        };
        let mut store = PairingStore::new();
        assert!(store
            .authorize_display_control(&device_id, &endpoint)
            .is_err());
        store.pair(&device_id, "ws-accept").unwrap();
        assert!(store
            .authorize_display_control(&device_id, &endpoint)
            .is_ok());
        let (width, height) = session
            .device
            .as_ref()
            .unwrap()
            .capabilities
            .surfaces
            .first()
            .map(|surface| (surface.width, surface.height))
            .unwrap();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width,
                height,
                bytes: vec![0x44; usize::from(width) * usize::from(height) * 2],
            })
            .unwrap();
        let mut idle = false;
        for _ in 0..80 {
            if session.poll().is_ok() && session.pending_request_count() == 0 {
                idle = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(idle, "websocket full frame was not acknowledged");
        session.disconnect();
        let _ = server.join();
    }

    #[test]
    fn mirror_command_writes_ppm_frames() {
        let dir = std::env::temp_dir().join(format!("mdc-cli-mirror-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let opts = GlobalOpts {
            sim: true,
            address: None,
            port: None,
            board: "linux-virt".into(),
        };
        let extra = vec![
            "--frames".to_string(),
            "1".to_string(),
            "--write-dir".to_string(),
            dir.to_string_lossy().into_owned(),
        ];
        cmd_mirror(&opts, &extra).unwrap();
        let written = std::fs::read(dir.join("frame.ppm")).unwrap();
        assert!(
            written.starts_with(b"P6\n800 480\n255\n"),
            "unexpected PPM header"
        );
        assert!(written.len() > 14);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn package_and_install_round_trip_on_a_sim_board() {
        let dir = std::env::temp_dir().join(format!("mdc-cli-pkg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let model = dir.join("sample.gltf");
        std::fs::write(
            &model,
            r#"{"asset":{"version":"2.0"},"nodes":[{"name":"root","mesh":0}],
                "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
                "accessors":[{"count":4,"type":"VEC3"}]}"#,
        )
        .unwrap();
        let out = dir.join("sample.mdcpkg");
        let opts = GlobalOpts {
            sim: true,
            address: None,
            port: None,
            board: "linux-virt".into(),
        };
        cmd_package(
            &opts,
            model.to_str().unwrap(),
            Some(out.to_string_lossy().into_owned()),
        )
        .unwrap();
        assert!(out.exists());
        // The linux-virt board advertises a mesh renderer, so install succeeds.
        cmd_install_package(&opts, out.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_command_sets_and_reads_back() {
        let opts = GlobalOpts {
            sim: true,
            address: None,
            port: None,
            board: "esp32-jc3248w535-sim".into(),
        };
        cmd_config(&opts, "set", "brightness", Some("200")).unwrap();
        cmd_config(&opts, "get", "brightness", None).unwrap();
        // A secret is redacted; the call still succeeds.
        cmd_config(&opts, "get", "wifi_password", None).unwrap();
    }

    #[test]
    fn scene_command_sets_and_clears() {
        let dir = std::env::temp_dir().join(format!("mdc-cli-scene-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("scene.json");
        std::fs::write(
            &path,
            r#"{"name":"root","kind":"rect","children":[{"name":"t","kind":"text","value":"hi"}]}"#,
        )
        .unwrap();
        let opts = GlobalOpts {
            sim: true,
            address: None,
            port: None,
            board: "linux-virt".into(),
        };
        cmd_scene(&opts, &[path.to_string_lossy().into_owned()]).unwrap();
        cmd_scene(&opts, &["clear".to_string()]).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scene_render_writes_a_ppm() {
        let dir = std::env::temp_dir().join(format!("mdc-cli-srender-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let scene = dir.join("scene.json");
        std::fs::write(
            &scene,
            r##"{"name":"bg","kind":"rect","rect":[0,0,4,4],"value":"#0000FF"}"##,
        )
        .unwrap();
        let out = dir.join("frame.ppm");
        let opts = GlobalOpts {
            sim: true,
            address: None,
            port: None,
            board: "esp32-jc3248w535-sim".into(),
        };
        cmd_scene_render(
            &opts,
            &[
                scene.to_string_lossy().into_owned(),
                "--out".to_string(),
                out.to_string_lossy().into_owned(),
            ],
        )
        .unwrap();
        let ppm = std::fs::read(&out).unwrap();
        assert!(ppm.starts_with(b"P6\n480 320\n255\n"));
        // The blue rect fills [0,0,4,4]; the first pixel is blue.
        assert_eq!(&ppm[15..18], &[0, 0, 255]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bake_command_writes_atlas_and_manifest() {
        use image::{Rgb, RgbImage};
        let dir = std::env::temp_dir().join(format!("mdc-cli-bake-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut sources = Vec::new();
        for (index, color) in [[255u8, 0, 0], [0, 255, 0]].iter().enumerate() {
            let image = RgbImage::from_pixel(8, 8, Rgb(*color));
            let path = dir.join(format!("src{index}.png"));
            image.save(&path).unwrap();
            sources.push(path.to_string_lossy().into_owned());
        }
        let out = dir.join("out");
        let mut args = sources;
        args.extend([
            "--frames".to_string(),
            "4".to_string(),
            "--sprite".to_string(),
            "4".to_string(),
            "--out".to_string(),
            out.to_string_lossy().into_owned(),
        ]);
        cmd_bake(&args).unwrap();
        let ppm = std::fs::read(out.join("atlas.ppm")).unwrap();
        assert!(
            ppm.starts_with(b"P6\n8 8\n255\n"),
            "4 frames of 4x4 -> 8x8 atlas"
        );
        let manifest = std::fs::read_to_string(out.join("atlas.json")).unwrap();
        assert!(manifest.contains("\"frames\": 4"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bake_package_installs_on_a_sim_board() {
        use image::{Rgb, RgbImage};
        let dir = std::env::temp_dir().join(format!("mdc-cli-bakepkg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("src.png");
        RgbImage::from_pixel(8, 8, Rgb([9, 9, 9]))
            .save(&source)
            .unwrap();
        let package_path = dir.join("baked.mdcpkg");
        let args = vec![
            source.to_string_lossy().into_owned(),
            "--frames".to_string(),
            "4".to_string(),
            "--sprite".to_string(),
            "4".to_string(),
            "--package".to_string(),
            package_path.to_string_lossy().into_owned(),
        ];
        cmd_bake(&args).unwrap();
        assert!(package_path.exists());

        let opts = GlobalOpts {
            sim: true,
            address: None,
            port: None,
            board: "esp32-jc3248w535-sim".into(),
        };
        cmd_install_package(&opts, package_path.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_mesh_and_package_write_ppms() {
        let dir = std::env::temp_dir().join(format!("mdc-cli-render-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let model_path = dir.join("rig.glb");
        std::fs::write(&model_path, skinned_glb()).unwrap();

        let mesh_out = dir.join("frame.ppm");
        cmd_render_mesh(&[
            model_path.to_string_lossy().into_owned(),
            "--time".to_string(),
            "0.5".to_string(),
            "--sprite".to_string(),
            "16".to_string(),
            "--out".to_string(),
            mesh_out.to_string_lossy().into_owned(),
        ])
        .unwrap();
        assert!(std::fs::read(&mesh_out)
            .unwrap()
            .starts_with(b"P6\n16 16\n255\n"));

        let package_path = dir.join("rig.mdcpkg");
        cmd_bake_mesh(&[
            model_path.to_string_lossy().into_owned(),
            "--frames".to_string(),
            "4".to_string(),
            "--sprite".to_string(),
            "16".to_string(),
            "--package".to_string(),
            package_path.to_string_lossy().into_owned(),
        ])
        .unwrap();
        let pkg_out = dir.join("pkg_frame.ppm");
        cmd_render_package(&[
            package_path.to_string_lossy().into_owned(),
            "--frame".to_string(),
            "2".to_string(),
            "--out".to_string(),
            pkg_out.to_string_lossy().into_owned(),
        ])
        .unwrap();
        assert!(std::fs::read(&pkg_out)
            .unwrap()
            .starts_with(b"P6\n16 16\n255\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn skinned_glb() -> Vec<u8> {
        fn pad4(data: &mut Vec<u8>, byte: u8) {
            while (data.len() & 3) != 0 {
                data.push(byte);
            }
        }
        let mut bin = Vec::new();
        for (x, y, z) in [(0.0f32, 0.0f32, 0.0f32), (1.0, 0.0, 0.0), (0.0, 1.0, 0.0)] {
            for v in [x, y, z] {
                bin.extend_from_slice(&v.to_le_bytes());
            }
        }
        for joints in [[0u16, 0, 0, 0], [1, 0, 0, 0], [0, 0, 0, 0]] {
            for j in joints {
                bin.extend_from_slice(&j.to_le_bytes());
            }
        }
        for _ in 0..3 {
            for w in [1.0f32, 0.0, 0.0, 0.0] {
                bin.extend_from_slice(&w.to_le_bytes());
            }
        }
        let identity = [
            1.0f32, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        for _ in 0..2 {
            for v in identity {
                bin.extend_from_slice(&v.to_le_bytes());
            }
        }
        for t in [0.0f32, 1.0] {
            bin.extend_from_slice(&t.to_le_bytes());
        }
        let quarter = std::f32::consts::FRAC_1_SQRT_2;
        for v in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, quarter, quarter] {
            bin.extend_from_slice(&v.to_le_bytes());
        }
        for (u, v) in [(0.0f32, 0.0f32), (1.0, 0.0), (0.0, 1.0)] {
            bin.extend_from_slice(&u.to_le_bytes());
            bin.extend_from_slice(&v.to_le_bytes());
        }
        let doc = serde_json::json!({
            "asset": {"version": "2.0"},
            "buffers": [{"byteLength": bin.len()}],
            "bufferViews": [
                {"buffer": 0, "byteOffset": 0, "byteLength": 36},
                {"buffer": 0, "byteOffset": 36, "byteLength": 24},
                {"buffer": 0, "byteOffset": 60, "byteLength": 48},
                {"buffer": 0, "byteOffset": 108, "byteLength": 128},
                {"buffer": 0, "byteOffset": 236, "byteLength": 8},
                {"buffer": 0, "byteOffset": 244, "byteLength": 32},
                {"buffer": 0, "byteOffset": 276, "byteLength": 24}
            ],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"},
                {"bufferView": 1, "componentType": 5123, "count": 3, "type": "VEC4"},
                {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC4"},
                {"bufferView": 3, "componentType": 5126, "count": 2, "type": "MAT4"},
                {"bufferView": 4, "componentType": 5126, "count": 2, "type": "SCALAR"},
                {"bufferView": 5, "componentType": 5126, "count": 2, "type": "VEC4"},
                {"bufferView": 6, "componentType": 5126, "count": 3, "type": "VEC2"}
            ],
            "meshes": [{"name": "body", "primitives": [{"attributes": {"POSITION": 0, "JOINTS_0": 1, "WEIGHTS_0": 2, "TEXCOORD_0": 6}}]}],
            "nodes": [
                {"name": "root", "mesh": 0, "children": [1]},
                {"name": "joint"}
            ],
            "skins": [{"name": "rig", "joints": [0, 1], "inverseBindMatrices": 3}],
            "animations": [{
                "name": "spin",
                "channels": [{"sampler": 0, "target": {"node": 1, "path": "rotation"}}],
                "samplers": [{"input": 4, "output": 5, "interpolation": "LINEAR"}]
            }]
        });
        let mut json = serde_json::to_vec(&doc).unwrap();
        pad4(&mut json, 0x20);
        pad4(&mut bin, 0x00);
        let total = 12 + 8 + json.len() + 8 + bin.len();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(json.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
        out.extend_from_slice(&json);
        out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x004E_4942u32.to_le_bytes());
        out.extend_from_slice(&bin);
        out
    }

    #[test]
    fn bake_mesh_writes_skinned_frames_and_packages() {
        let dir = std::env::temp_dir().join(format!("mdc-cli-bakemesh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let model_path = dir.join("rig.glb");
        std::fs::write(&model_path, skinned_glb()).unwrap();

        let texture_path = dir.join("tex.png");
        image::RgbImage::from_pixel(2, 2, image::Rgb([200, 40, 40]))
            .save(&texture_path)
            .unwrap();

        let out_dir = dir.join("frames");
        cmd_bake_mesh(&[
            model_path.to_string_lossy().into_owned(),
            "--frames".to_string(),
            "4".to_string(),
            "--sprite".to_string(),
            "16".to_string(),
            "--texture".to_string(),
            texture_path.to_string_lossy().into_owned(),
            "--out".to_string(),
            out_dir.to_string_lossy().into_owned(),
        ])
        .unwrap();
        let first = std::fs::read(out_dir.join("mesh_frame_00.ppm")).unwrap();
        let last = std::fs::read(out_dir.join("mesh_frame_03.ppm")).unwrap();
        assert!(first.starts_with(b"P6\n16 16\n255\n"));
        assert_ne!(first, last, "the pose changes across frames");

        let package_path = dir.join("rig.mdcpkg");
        cmd_bake_mesh(&[
            model_path.to_string_lossy().into_owned(),
            "--frames".to_string(),
            "4".to_string(),
            "--sprite".to_string(),
            "16".to_string(),
            "--package".to_string(),
            package_path.to_string_lossy().into_owned(),
        ])
        .unwrap();
        let opts = GlobalOpts {
            sim: true,
            address: None,
            port: None,
            board: "esp32-jc3248w535-sim".into(),
        };
        cmd_install_package(&opts, package_path.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
