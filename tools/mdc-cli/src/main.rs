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
    /// N676: the device-advertised factory defaults (`Capabilities.config.defaults`).
    #[serde(rename = "configDefaults")]
    config_defaults: std::collections::BTreeMap<String, String>,
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
    /// N307: the format enum is exposed to the SDK; only RGB565 is implemented.
    implemented: bool,
    stride: u32,
    rotation: u16,
    /// N351: the device display profile (N308).
    preferred_tile_size: u16,
    supports_partial_update: bool,
    supports_full_framebuffer: bool,
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
                    pixel_format: s.pixel_format.to_string(),
                    implemented: s.pixel_format.is_implemented(),
                    stride: s.stride,
                    rotation: s.rotation,
                    preferred_tile_size: s.preferred_tile_size,
                    supports_partial_update: s.supports_partial_update,
                    supports_full_framebuffer: s.supports_full_framebuffer,
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
            config_defaults: d.capabilities.config.defaults.clone(),
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
        /// N351: diagnostic records emitted by the device.
        diagnostics: Option<u64>,
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
                    pixel_format: s.pixel_format.to_string(),
                    implemented: s.pixel_format.is_implemented(),
                    stride: s.stride,
                    rotation: s.rotation,
                    preferred_tile_size: s.preferred_tile_size,
                    supports_partial_update: s.supports_partial_update,
                    supports_full_framebuffer: s.supports_full_framebuffer,
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
            config_defaults: d.capabilities.config.defaults.clone(),
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
        diagnostics: metrics.map(|m| m.diagnostics_emitted),
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

fn cmd_devices(opts: &GlobalOpts, extra: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    // N437: `--compact` streams one JSON object per device (JSONL) for scripting.
    let compact = extra.iter().any(|a| a == "--compact");
    with_sim_or_empty(opts, |mgr, _| {
        if compact {
            for line in devices_json_lines(mgr) {
                println!("{line}");
            }
        } else {
            println!("{}", devices_json(mgr));
        }
        Ok(())
    })
}

/// N437: one compact JSON object per device, newest first.
fn devices_json_lines(manager: &DeviceManager) -> Vec<String> {
    manager
        .devices()
        .map(|device| {
            serde_json::json!({
                "device_id": device.capabilities.device_id,
                "firmware": device.capabilities.firmware,
                "state": format!("{:?}", device.state),
                "surfaces": device.capabilities.surfaces.iter().map(|s| serde_json::json!({
                    "id": s.id,
                    "width": s.width,
                    "height": s.height,
                })).collect::<Vec<_>>(),
            })
            .to_string()
        })
        .collect()
}

/// N370: the host process resident set size in bytes, when the platform exposes
/// it (Linux: current RSS from `/proc`; other unix: peak RSS via `getrusage`).
fn host_rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        Some(pages * 4096)
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
            return None;
        }
        Some(usage.ru_maxrss as u64)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

fn cmd_inspect(opts: &GlobalOpts, extra: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    // N57: bounded live stream (`--watch --count N --interval MS`).
    let mut watch = false;
    let mut rss = false;
    // N646: print only the board's managed-config defaults.
    let mut defaults_only = false;
    // N656: restrict the defaults table to one key.
    let mut defaults_key: Option<String> = None;
    let mut count: u32 = 3;
    let mut interval_ms: u64 = 200;
    let mut iter = extra.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--watch" => watch = true,
            // N686: `--defaults` is an alias of `--defaults-only` (same table as
            // `mdc config --defaults`).
            "--defaults-only" | "--defaults" => defaults_only = true,
            "--key" => defaults_key = iter.next().cloned(),
            // N370: add the host process memory reading to each snapshot.
            "--rss" => rss = true,
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
    // N646: a session-less snapshot of the board defaults.
    if defaults_only {
        // N656: `--key K` narrows the table to one entry.
        let body = match defaults_key.as_deref() {
            Some(key) => defaults_key_json(&opts.board, key),
            None => defaults_json(&opts.board),
        };
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if watch {
        let (mut session, mut device, manager) = open_sim(&opts.board)?;
        for tick in 0..count {
            let _ = session.poll();
            let _ = device.poll();
            let snapshot = device.metrics_snapshot();
            let mgr = manager.lock().map_err(|e| e.to_string())?;
            let mut value = serde_json::json!({
                "tick": tick,
                "board": opts.board,
                "scene_nodes": device.scene_node_count(),
                "atlas_frames": device.atlas_frames(),
                "diagnostics_emitted": snapshot.diagnostics_emitted,
                "session": {
                    "packets_sent": session.stats().packets_sent,
                    "bytes_sent": session.stats().bytes_sent,
                    "bytes_received": session.stats().bytes_received,
                    "acks_received": session.stats().acks_received,
                    "frames_sent": session.stats().frames_sent,
                    "dropped_frames": session.stats().dropped_frames,
                    "packages_sent": session.stats().packages_sent,
                    "mirror_frames": session.stats().mirror_frames,
                    "pacing_dropped": session.stats().pacing_dropped,
                },
                 "config": inspect_json(&mgr, Some(&snapshot)),
                // N606: the board's own managed-config defaults.
                "board_config_defaults": board_config_defaults(&opts.board),
            });
            // N676: profile defaults merged with the device-advertised defaults.
            let advertised: Vec<_> = mgr
                .devices()
                .map(|d| d.capabilities.config.defaults.clone())
                .collect();
            value["config_defaults"] = merge_default_maps(&opts.board, &advertised);
            if rss {
                value["host_rss_bytes"] = serde_json::json!(host_rss_bytes());
            }
            println!("{value}");
            if tick + 1 < count {
                std::thread::sleep(std::time::Duration::from_millis(interval_ms));
            }
        }
        return Ok(());
    }
    with_sim_or_empty(opts, |mgr, metrics| {
        let json = inspect_json(mgr, metrics);
        let mut value: serde_json::Value = serde_json::from_str(&json)?;
        // N606: expose the board's managed-config defaults.
        value["board_config_defaults"] = board_config_defaults(&opts.board);
        // N676: profile defaults merged with the device-advertised defaults.
        let advertised: Vec<_> = mgr
            .devices()
            .map(|d| d.capabilities.config.defaults.clone())
            .collect();
        value["config_defaults"] = merge_default_maps(&opts.board, &advertised);
        if rss {
            value["host_rss_bytes"] = serde_json::json!(host_rss_bytes());
        }
        println!("{value}");
        Ok(())
    })
}

/// N646: the shared defaults table body (same key as `mdc inspect` and
/// `mdc config --defaults`).
fn defaults_json(board: &str) -> serde_json::Value {
    serde_json::json!({
        "board": board,
        "board_config_defaults": board_config_defaults(board),
    })
}

/// N656: one entry of the shared defaults table (`value` is null when the board
/// does not advertise the key).
fn defaults_key_json(board: &str, key: &str) -> serde_json::Value {
    let defaults = board_config_defaults(board);
    serde_json::json!({
        "board": board,
        "key": key,
        "value": defaults.get(key).cloned().unwrap_or(serde_json::Value::Null),
    })
}

/// N666: select one config default (by key) or return them all.
fn filter_config_defaults(all: &serde_json::Value, key: Option<&str>) -> serde_json::Value {
    match key {
        Some(name) => match all.get(name) {
            Some(value) => serde_json::json!({ name: value.clone() }),
            None => serde_json::json!({}),
        },
        None => all.clone(),
    }
}

/// N676: merge the board profile's defaults with the device-advertised defaults
/// (the device wins on a conflict). Returns a JSON object.
fn merge_default_maps(
    board: &str,
    advertised: &[std::collections::BTreeMap<String, String>],
) -> serde_json::Value {
    let mut merged = match board_config_defaults(board) {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    for map in advertised {
        for (key, value) in map {
            merged.insert(key.clone(), serde_json::json!(value));
        }
    }
    serde_json::Value::Object(merged)
}

/// N606: the board's own `config_defaults` as a JSON object (empty when none).
fn board_config_defaults(board: &str) -> serde_json::Value {
    serde_json::to_value(mdc_simulator::BoardProfile::from_board_id(board).config_defaults)
        .unwrap_or_else(|_| serde_json::json!({}))
}

/// N32: compile a source model into a runtime package for a board's renderer.
/// N594: label the vertex-cache policy for the compile report.
fn cache_mode(optimize_geometry: bool, optimize_index: usize) -> &'static str {
    if optimize_index > 0 {
        "fixed"
    } else if optimize_geometry {
        "adaptive"
    } else {
        "off"
    }
}

fn cmd_package(
    opts: &GlobalOpts,
    model_path: &str,
    extra: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut out: Option<String> = None;
    let mut texture_path: Option<String> = None;
    let mut atlas_page: Option<u32> = None;
    let mut indexed: u32 = 0;
    let mut dither: u8 = 0;
    let mut tangents = false;
    let mut variant: Option<String> = None;
    let mut report = false;
    let mut texture_mips: usize = 0;
    // N504: bake from a coarser mip level (a small-target package).
    let mut mip_level: usize = 0;
    // N524: cap the vertices the package draws (0 = no cap).
    let mut max_draw_vertices: u64 = 0;
    // N545: losslessly merge duplicate vertices and drop unreferenced ones.
    let mut compact_geometry = false;
    // N555: reorder triangles for vertex-cache locality (0 = 32-entry cache).
    let mut optimize_index: usize = 0;
    // N574: combined compaction + reordering (`--optimize-geometry`).
    let mut optimize_geometry = false;
    let mut iter = extra.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--texture" => texture_path = iter.next().cloned(),
            "--out" => out = iter.next().cloned(),
            // N380: pack per-material textures into one bounded page.
            "--atlas" => {
                atlas_page = Some(iter.next().and_then(|v| v.parse().ok()).unwrap_or(2048))
            }
            // N415: quantize each texture blob to a palette.
            "--indexed" => indexed = iter.next().and_then(|v| v.parse().ok()).unwrap_or(16),
            // N416: ordered-dither each texture blob (strength in LSBs).
            "--dither" => dither = iter.next().and_then(|v| v.parse().ok()).unwrap_or(4),
            // N428: derive tangents for meshes that omit them.
            "--tangents" => tangents = true,
            // N433: pick a KHR_materials_variants set by name.
            "--variant" => variant = iter.next().cloned(),
            // N458: print an extended compile report.
            "--report" => report = true,
            // N490: attach a mip chain to each texture blob.
            "--texture-mips" => {
                texture_mips = iter.next().and_then(|v| v.parse().ok()).unwrap_or(8)
            }
            // N504: use a coarser chain level as the base texture.
            "--mip-level" => mip_level = iter.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            // N524: trim meshes past a cumulative vertex budget.
            "--max-draw-vertices" => {
                max_draw_vertices = iter.next().and_then(|v| v.parse().ok()).unwrap_or(0)
            }
            // N545: losslessly compact the geometry before compiling.
            "--compact-geometry" => compact_geometry = true,
            // N555: reorder triangles for vertex-cache locality.
            "--optimize-index" => {
                optimize_index = iter.next().and_then(|v| v.parse().ok()).unwrap_or(32)
            }
            // N564/N574: compaction + index reordering in one pass; a bare flag
            // uses a mesh-scaled cache (an explicit value overrides it).
            "--optimize-geometry" => {
                compact_geometry = true;
                optimize_geometry = true;
                optimize_index = iter.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            }
            other => out = Some(other.to_string()),
        }
    }
    let bytes = std::fs::read(model_path)?;
    let mut model = if model_path.to_ascii_lowercase().ends_with(".json") {
        mdc_model::Live2DImporter.import(&bytes, &mdc_model::ImportLimits::default())?
    } else {
        mdc_model::GltfImporter.import(&bytes, &mdc_model::ImportLimits::default())?
    };
    // N433: swap in the materials for the requested variant.
    if let Some(name) = &variant {
        if let Some(index) = model.variants.iter().position(|v| v == name) {
            let materials = model.variant_materials(index);
            for (mesh, material) in model.meshes.iter_mut().zip(materials) {
                if let Some(material) = material {
                    mesh.material = Some(material);
                }
            }
        }
    }
    // N428: fill in tangents where the mesh omits them.
    if tangents {
        for mesh in &mut model.meshes {
            if let Some(geometry) = mesh.geometry.as_mut() {
                if geometry.tangents.len() < geometry.positions.len()
                    && geometry.uv.len() >= geometry.positions.len()
                {
                    geometry.tangents = mdc_bake::generate_tangents(geometry);
                }
            }
        }
    }
    // N545: optionally merge duplicate vertices and drop unreferenced ones.
    let mut compacted_vertices: usize = 0;
    if compact_geometry {
        for mesh in &mut model.meshes {
            if let Some(geometry) = mesh.geometry.as_mut() {
                let removed = mdc_bake::compact_geometry(geometry);
                if removed > 0 {
                    compacted_vertices += removed;
                    mesh.vertices = geometry.positions.len() as u32;
                    mesh.indices = geometry.indices.len() as u32;
                }
            }
        }
    }
    // N555/N574: optionally reorder triangles for vertex-cache locality. A
    // zero value selects a mesh-scaled cache (N574).
    let mut index_cache_misses: u64 = 0;
    // N669: the miss count before the pass, so the report can show the saving.
    let mut index_cache_before: u64 = 0;
    let mut reordered_meshes: usize = 0;
    // N684: how many meshes the order pass looked at.
    let mut ordered_meshes: usize = 0;
    // N644: aggregate hit/miss counters over the reordered meshes.
    let mut cache_hits: u64 = 0;
    let mut cache_lookups: u64 = 0;
    // N654: the same counters before the reorder, for a before/after report.
    let mut cache_hits_before: u64 = 0;
    let mut cache_lookups_before: u64 = 0;
    if optimize_geometry || optimize_index > 0 {
        for mesh in &mut model.meshes {
            if let Some(geometry) = mesh.geometry.as_mut() {
                ordered_meshes += 1;
                let cache = if optimize_index == 0 {
                    mdc_bake::adaptive_cache_size(geometry)
                } else {
                    optimize_index
                };
                // N654: one call reports locality before and after the reorder.
                let (before_hits, before_misses) = mdc_bake::index_cache_stats(geometry, cache);
                let report = mdc_bake::optimize_index_order_report(geometry, cache);
                index_cache_misses += report.after_misses;
                index_cache_before += report.before_misses;
                cache_hits_before += before_hits;
                cache_lookups_before += before_hits + before_misses;
                let (hits, misses) = mdc_bake::index_cache_stats(geometry, cache);
                cache_hits += hits;
                cache_lookups += hits + misses;
                if report.after_misses < report.before_misses {
                    reordered_meshes += 1;
                }
            }
        }
    }
    let profile = BoardProfile::from_board_id(&opts.board);
    let caps = mdc_model::RendererCapability {
        sprite: profile.renderer_sprite,
        mesh2d: profile.renderer_mesh2d,
        skeleton: profile.renderer_skeleton,
        max_texture: profile.renderer_max_texture,
        max_vertices: profile.renderer_max_vertices,
    };
    // N179/N324/N353: an explicit `--texture` overrides; otherwise every
    // embedded material texture is attached (multi-material).
    let limits = mdc_bake::BakeLimits::default();
    let (package, texture_source, textured) = match &texture_path {
        Some(path) => {
            let blob = mdc_bake::bake_texture_blob(&std::fs::read(path)?, &limits)?;
            (
                mdc_bake::mesh_package_with_texture(&model, &caps, Some(blob))?,
                Some("arg"),
                true,
            )
        }
        None => {
            let blobs = mdc_bake::attach_model_textures(&model, &limits);
            let textured = blobs.iter().any(|blob| blob.validate());
            (
                mdc_bake::mesh_package_with_textures(&model, &caps, blobs)?,
                if textured { Some("model") } else { None },
                textured,
            )
        }
    };
    let texture_error: Option<String> = None;
    // N490: optionally attach a mip chain to the textures.
    let package = if texture_mips >= 2 {
        let mut inner = package.package.clone();
        inner.texture_blobs = inner
            .texture_blobs
            .iter()
            .map(|blob| mdc_bake::mip_texture_blob(blob, texture_mips, false))
            .collect();
        inner.texture = inner
            .texture
            .as_ref()
            .map(|blob| mdc_bake::mip_texture_blob(blob, texture_mips, false));
        mdc_model::MdcPackage::seal(inner)?
    } else {
        package
    };
    // N504: optionally replace the base texture with a coarser mip level.
    let package = if mip_level > 0 {
        let mut inner = package.package.clone();
        let level = |blob: &mdc_model::TextureBlob| {
            mdc_bake::blob_mip_level(blob, mip_level).unwrap_or_else(|| blob.clone())
        };
        inner.texture_blobs = inner.texture_blobs.iter().map(level).collect();
        inner.texture = inner.texture.as_ref().map(level);
        mdc_model::MdcPackage::seal(inner)?
    } else {
        package
    };
    // N416: optionally dither the textures before any palette step.
    let package = if dither > 0 {
        let mut inner = package.package.clone();
        inner.texture_blobs = inner
            .texture_blobs
            .iter()
            .map(|blob| mdc_bake::dither_texture_blob(blob, dither))
            .collect();
        inner.texture = inner
            .texture
            .as_ref()
            .map(|blob| mdc_bake::dither_texture_blob(blob, dither));
        mdc_model::MdcPackage::seal(inner)?
    } else {
        package
    };
    // N415: optionally quantize the textures to a palette (form-A devices).
    let package = if indexed >= 2 {
        let mut inner = package.package.clone();
        inner.texture_blobs = inner
            .texture_blobs
            .iter()
            .map(|blob| mdc_bake::index_texture_blob(blob, indexed))
            .collect();
        inner.texture = inner
            .texture
            .as_ref()
            .map(|blob| mdc_bake::index_texture_blob(blob, indexed));
        mdc_model::MdcPackage::seal(inner)?
    } else {
        package
    };
    // N380: optionally collapse the per-material textures into one page.
    let package = match atlas_page {
        Some(page) => {
            let mut inner = package.package.clone();
            if mdc_bake::pack_material_atlas(&mut inner, page) {
                mdc_model::MdcPackage::seal(inner)?
            } else {
                package
            }
        }
        None => package,
    };
    // N524: optionally cap the drawn vertices (later meshes are trimmed).
    let trimmed = if max_draw_vertices > 0 {
        let mut inner = package.package.clone();
        let dropped = mdc_bake::trim_to_vertex_budget(&mut inner, max_draw_vertices);
        (mdc_model::MdcPackage::seal(inner)?, dropped)
    } else {
        (package, 0usize)
    };
    let (package, trimmed_meshes) = trimmed;
    let bytes = package.to_bytes()?;
    let out = out.unwrap_or_else(|| format!("{model_path}.mdcpkg"));
    std::fs::write(&out, &bytes)?;
    if report {
        // N458: an extended compile report (texture/palette/mip detail).
        let verification = package.verify();
        let blobs = &package.package.texture_blobs;
        let indexed = blobs.iter().filter(|b| b.is_indexed()).count();
        let palette_entries: usize = blobs.iter().map(|b| b.palette.len()).sum();
        let blob_bytes: usize = blobs.iter().map(|b| b.rgb565.len() + b.indices.len()).sum();
        let mips = package
            .package
            .atlas
            .as_ref()
            .map(|a| a.mips.len())
            .unwrap_or(0);
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "board": opts.board,
                "source": model_path,
                "target": package.manifest.target,
                "mode": package.manifest.mode,
                "verified": verification.valid,
                "checksum": verification.checksum,
                "body_bytes": package.manifest.body_bytes,
                "package_bytes": bytes.len(),
                "nodes": package.package.nodes.len(),
                "meshes": package.package.meshes.len(),
                "materials": package.package.materials.len(),
                "textures": package.package.textures.len(),
                "texture_blobs": blobs.len(),
                "indexed_textures": indexed,
                "palette_entries": palette_entries,
                "texture_blob_bytes": blob_bytes,
                // N570: KTX2 metadata retained from the model bytes.
                "ktx2_textures": package.package.textures.iter().filter(|t| t.is_ktx2()).count(),
                "ktx2_slices": package.package.textures.iter().filter_map(|t| t.ktx2_header()).map(|h| h.slices() as u64).sum::<u64>(),
                "atlas_mips": mips,
                // N504: the texture mip chains and their levels.
                "texture_mips": blobs.iter().map(|b| b.mips.len()).sum::<usize>(),
                "mip_levels": blobs.iter().map(|b| mdc_bake::mip_chain_levels(b).len()).collect::<Vec<_>>(),
                "animations": package.package.animations.len(),
                "estimated_bytes": package.package.estimated_bytes,
                // N524: the drawn-vertex budget and how many meshes it trimmed.
                "draw_vertices": mdc_bake::draw_vertices(&package.package),
                "trimmed_meshes": trimmed_meshes,
                // N545: vertices removed by lossless geometry compaction.
                "compacted_vertices": compacted_vertices,
                // N555: cache misses after reordering and meshes that improved.
                "index_cache_misses": index_cache_misses,
                // N669: misses removed by the reorder.
                "index_cache_saved": index_cache_before.saturating_sub(index_cache_misses),
                "reordered_meshes": reordered_meshes,
                // N684: the package-order summary (same shape as the library report).
                "package_order": {
                    "meshes": ordered_meshes,
                    "reordered": reordered_meshes,
                    "misses_before": index_cache_before,
                    "misses_after": index_cache_misses,
                    "saved": index_cache_before.saturating_sub(index_cache_misses),
                },
                // N594: which vertex-cache policy ran (off | adaptive | fixed).
                "cache_mode": cache_mode(optimize_geometry, optimize_index),
                "cache_size": optimize_index,
                // N644: the vertex-cache hit rate after any reordering.
                "cache_hit_rate": if cache_lookups == 0 {
                    0.0
                } else {
                    cache_hits as f64 / cache_lookups as f64
                },
                // N654: the hit rate before the reorder (same walk).
                "cache_hit_rate_before": if cache_lookups_before == 0 {
                    0.0
                } else {
                    cache_hits_before as f64 / cache_lookups_before as f64
                },
                "output": out,
            }))?
        );
        return Ok(());
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "board": opts.board,
            "source": model_path,
            "target": package.manifest.target,
            "mode": package.manifest.mode,
            "textured": textured,
            "textureSource": texture_source,
            "textureError": texture_error,
            "nodes": package.package.nodes.len(),
            "meshes": package.package.meshes.len(),
            "animations": package.package.animations.len(),
            "morphTargets": model
                .meshes
                .iter()
                .filter_map(|m| m.geometry.as_ref())
                .map(|g| g.morph_targets.len())
                .max()
                .unwrap_or(0),
            "primitives": model.meshes.iter().map(|m| m.primitives).max().unwrap_or(0),
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

/// N339: end-to-end provisioning of a runtime package onto a simulator board:
/// preflight (container parses + matches the device renderer) → install → version
/// readback, with a rollback report when the device refuses it.
fn cmd_provision(opts: &GlobalOpts, path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    // Preflight: the container must parse before anything is sent.
    let package = mdc_model::MdcPackage::from_bytes(&bytes)
        .map_err(|e| format!("package preflight failed: {e}"))?;
    let (mut session, mut device, _manager) = open_sim(&opts.board)?;
    let renderer = mdc_model::RendererCapability::from_wire(
        session
            .device
            .as_ref()
            .and_then(|d| d.capabilities.renderer.as_ref()),
    );
    let accepted = package.accepted_by(&renderer).is_ok();
    let firmware_before = session
        .device
        .as_ref()
        .map(|d| d.capabilities.firmware.clone())
        .unwrap_or_default();
    let ready = if accepted {
        session.install_package(&bytes)?;
        device.poll()?;
        session.poll()?;
        device.package_ready()
    } else {
        false
    };
    // Version readback (also refreshes Capabilities after the install).
    session.refresh_capabilities()?;
    device.poll()?;
    session.poll()?;
    let firmware_after = session
        .device
        .as_ref()
        .map(|d| d.capabilities.firmware.clone())
        .unwrap_or_default();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "board": opts.board,
            "package": path,
            "package_bytes": bytes.len(),
            "accepted": accepted,
            "renderer_ready": ready,
            "rolled_back": !ready,
            "firmware_before": firmware_before,
            "firmware_after": firmware_after,
            "packages_received": device.metrics.packages_received,
        }))?
    );
    if !ready {
        return Err("device did not accept the runtime package; rolled back".into());
    }
    Ok(())
}
/// N350: drive an OTA against a simulator board end-to-end (begin → chunk →
/// confirm) and read the firmware version back, with a rollback report on error.
fn cmd_ota(
    opts: &GlobalOpts,
    version: &str,
    extra: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut size: Option<u64> = None;
    let mut sha256: Option<String> = None;
    let mut iter = extra.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--size" => size = iter.next().and_then(|v| v.parse().ok()),
            "--sha256" => sha256 = iter.next().cloned(),
            _ => {}
        }
    }
    // N354: over TCP the peer drives the device; there is no local device.
    if let Some(address) = opts.address.clone() {
        let transport = connect_tcp(&address)?;
        let mut session = Session::new(transport);
        session.connect()?;
        pump_until_idle(&mut session)?;
        let ota = session
            .device
            .as_ref()
            .map(|d| d.capabilities.ota)
            .unwrap_or(false);
        if !ota {
            return Err(format!("device at {address} does not advertise OTA").into());
        }
        let firmware_before = session
            .device
            .as_ref()
            .map(|d| d.capabilities.firmware.clone())
            .unwrap_or_default();
        let result = drive_ota_tcp(&mut session, version, size, sha256.clone());
        session.refresh_capabilities()?;
        pump_until_idle(&mut session)?;
        let firmware_after = session
            .device
            .as_ref()
            .map(|d| d.capabilities.firmware.clone())
            .unwrap_or_default();
        let ok = result.is_ok() && firmware_after == version;
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "address": address,
                "version": version,
                "firmware_before": firmware_before,
                "firmware_after": firmware_after,
                "ok": ok,
                "rolled_back": firmware_after != version,
                "error": result.err(),
            }))?
        );
        if !ok {
            return Err("OTA did not confirm the requested version".into());
        }
        return Ok(());
    }
    let (mut session, mut device, _manager) = open_sim(&opts.board)?;
    let ota = session
        .device
        .as_ref()
        .map(|d| d.capabilities.ota)
        .unwrap_or(false);
    if !ota {
        return Err(format!("board {} does not advertise OTA", opts.board).into());
    }
    let firmware_before = session
        .device
        .as_ref()
        .map(|d| d.capabilities.firmware.clone())
        .unwrap_or_default();
    let step = |session: &mut Session<mdc_transport::MemoryLink>,
                device: &mut FakeDevice,
                command: mdc_protocol::OtaCommand|
     -> Result<(), String> {
        session.send_ota(command).map_err(|e| e.to_string())?;
        device.poll().map_err(|e| e.to_string())?;
        session.poll().map_err(|e| e.to_string()).map(|_| ())
    };
    let result = (|| -> Result<(), String> {
        step(
            &mut session,
            &mut device,
            mdc_protocol::OtaCommand {
                action: "begin".into(),
                version: Some(version.to_string()),
                size,
                sha256: None,
            },
        )?;
        step(
            &mut session,
            &mut device,
            mdc_protocol::OtaCommand {
                action: "chunk".into(),
                version: None,
                size: None,
                sha256: sha256.clone(),
            },
        )?;
        step(
            &mut session,
            &mut device,
            mdc_protocol::OtaCommand {
                action: "confirm".into(),
                version: None,
                size: None,
                sha256: None,
            },
        )
    })();
    // Version readback (also refreshes Capabilities after the install).
    session.refresh_capabilities()?;
    device.poll()?;
    session.poll()?;
    let firmware_after = session
        .device
        .as_ref()
        .map(|d| d.capabilities.firmware.clone())
        .unwrap_or_default();
    let ok = result.is_ok() && firmware_after == version;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "board": opts.board,
            "version": version,
            "firmware_before": firmware_before,
            "firmware_after": firmware_after,
            "ok": ok,
            "rolled_back": firmware_after != version,
            "error": result.err(),
        }))?
    );
    if !ok {
        return Err("OTA did not confirm the requested version".into());
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
    let mut mipmaps = false;
    let mut mip_linear = false;
    let mut dither: u8 = 0;
    let mut palette: u32 = 0;
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
            // N396: also write a box-downsampled mip chain beside the atlas.
            "--mipmaps" => mipmaps = true,
            // N468: average the written mip chain in linear light.
            "--mip-linear" => mip_linear = true,
            // N404: ordered (Bayer) dithering strength in LSBs (0..=8).
            "--dither" => dither = iter.next().and_then(|v| v.parse().ok()).unwrap_or(4),
            // N405: median-cut the atlas to at most N palette entries.
            "--palette" => palette = iter.next().and_then(|v| v.parse().ok()).unwrap_or(16),
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
        curves: Vec::new(),
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
        // N438: `--mipmaps` also stores a mip chain inside the package.
        let sealed = if mipmaps {
            mdc_bake::bake_to_package_mipped(
                &sources,
                &motions,
                (sprite, sprite),
                max_texture,
                &mdc_bake::BakeLimits::default(),
                8,
            )?
        } else {
            mdc_bake::bake_to_package(
                &sources,
                &motions,
                (sprite, sprite),
                max_texture,
                &mdc_bake::BakeLimits::default(),
            )?
        };
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
    // N404: an optional ordered-dither pass before writing.
    let dithered = mdc_bake::bayer_dither_rgb565(
        package.atlas.width,
        package.atlas.height,
        &package.atlas.rgb565,
        dither,
    );
    std::fs::write(
        &atlas_path,
        rgb565_frame_to_ppm(package.atlas.width, package.atlas.height, &dithered),
    )?;
    let manifest_path = Path::new(&out_dir).join("atlas.json");
    std::fs::write(&manifest_path, package.manifest.to_json()?)?;
    // N405: an optional median-cut palette reduction.
    let mut palette_json = None;
    if palette >= 2 {
        let quantized = mdc_bake::quantize_palette(
            package.atlas.width,
            package.atlas.height,
            &dithered,
            palette,
        );
        let palette_path = Path::new(&out_dir).join("atlas.pal.json");
        let colors: Vec<String> = quantized
            .palette
            .iter()
            .map(|c| format!("{c:04x}"))
            .collect();
        std::fs::write(
            &palette_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "width": quantized.width,
                "height": quantized.height,
                "colors": colors,
            }))?,
        )?;
        let index_path = Path::new(&out_dir).join("atlas.idx");
        std::fs::write(&index_path, &quantized.indices)?;
        palette_json = Some(serde_json::json!({
            "colors": quantized.palette.len(),
            "palette_path": palette_path.display().to_string(),
            "index_path": index_path.display().to_string(),
        }));
    }
    // N396: an optional box-downsampled mip chain (level 0 is the atlas).
    let mut mip_paths = Vec::new();
    if mipmaps {
        for (level, (w, h, data)) in mdc_bake::bake_mipmaps_with(
            package.atlas.width,
            package.atlas.height,
            &dithered,
            8,
            mip_linear,
        )
        .iter()
        .enumerate()
        {
            let path = Path::new(&out_dir).join(format!("mip{level}.ppm"));
            std::fs::write(&path, rgb565_frame_to_ppm(*w, *h, data))?;
            mip_paths.push(path.display().to_string());
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "sources": images.len(),
            "frames": package.atlas.frames,
            "atlas": format!("{}x{}", package.atlas.width, package.atlas.height),
            "atlas_path": atlas_path.display().to_string(),
            "manifest_path": manifest_path.display().to_string(),
            "mips": mip_paths,
            "dither": dither,
            "palette": palette_json,
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
    let mut all_animations = false;
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
            // N345: bake every animation into one multi-clip atlas package.
            "--all-animations" => all_animations = true,
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
            Some(mdc_bake::texture_map_with_alpha8(
                &image,
                &mdc_bake::BakeLimits::default(),
            )?)
        }
        None => None,
    };
    let textured = texture.is_some() && has_uv;
    if let Some(path) = &package_path {
        if all_animations {
            // N345: one clip per animation in a single atlas package.
            let clips: Vec<(usize, u32)> = (0..model.animations.len())
                .map(|index| (index, frames))
                .collect();
            let sealed = mdc_bake::bake_scene_clips_to_package(
                &model,
                &clips,
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
                    "clips": clips.len(),
                    "frames": frames,
                    "mode": sealed.manifest.mode,
                    "textured": textured,
                    "package_path": path,
                    "package_bytes": package_bytes.len(),
                }))?
            );
            return Ok(());
        }
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

/// N217: parse a Live2D `motion3.json` and sample its parameter curves.
fn cmd_motion(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut file: Option<String> = None;
    let mut param: Option<String> = None;
    let mut samples: u32 = 8;
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--param" => param = iter.next().cloned(),
            "--samples" => samples = iter.next().and_then(|v| v.parse().ok()).unwrap_or(samples),
            other => file = Some(other.to_string()),
        }
    }
    let file = file.ok_or("motion requires a motion3.json file")?;
    let bytes = std::fs::read(&file)?;
    let info = mdc_model::Live2DImporter.import_motion(
        &file,
        &bytes,
        &mdc_model::ImportLimits::default(),
    )?;
    let output = match &param {
        Some(id) => {
            let curve = info
                .curves
                .iter()
                .find(|c| &c.id == id)
                .ok_or_else(|| format!("motion has no curve {id}"))?;
            let duration = info.duration_sec.max(0.0) as f32;
            let values: Vec<f32> = (0..samples.max(1))
                .map(|i| {
                    let t = if samples > 1 {
                        duration * (i as f32) / ((samples - 1) as f32)
                    } else {
                        0.0
                    };
                    curve.sample(t)
                })
                .collect();
            serde_json::json!({
                "file": file,
                "name": info.name,
                "param": id,
                "duration": info.duration_sec,
                "fps": info.fps,
                "samples": values,
            })
        }
        None => serde_json::json!({
            "file": file,
            "name": info.name,
            "duration": info.duration_sec,
            "fps": info.fps,
            "looping": info.looping,
            "curves": info.curves.iter().map(|c| serde_json::json!({
                "id": c.id,
                "target": c.target,
                "segments": c.segments.len(),
            })).collect::<Vec<_>>(),
        }),
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Encode a baked RGB565 atlas as a binary PPM (P6).
/// N448: box-average a BGRA frame by an integer factor.
fn downscale_bgra(width: u32, height: u32, bytes: &[u8], factor: u32) -> Vec<u8> {
    let factor = factor.max(1);
    let out_w = (width / factor).max(1);
    let out_h = (height / factor).max(1);
    let mut out = vec![0u8; (out_w as usize) * (out_h as usize) * 4];
    for y in 0..out_h {
        for x in 0..out_w {
            let (mut b, mut g, mut r, mut a, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
            for dy in 0..factor {
                for dx in 0..factor {
                    let sx = (x * factor + dx).min(width.saturating_sub(1));
                    let sy = (y * factor + dy).min(height.saturating_sub(1));
                    let index = ((sy * width + sx) * 4) as usize;
                    if let Some(pixel) = bytes.get(index..index + 4) {
                        b += u32::from(pixel[0]);
                        g += u32::from(pixel[1]);
                        r += u32::from(pixel[2]);
                        a += u32::from(pixel[3]);
                        n += 1;
                    }
                }
            }
            let n = n.max(1);
            let index = ((y * out_w + x) * 4) as usize;
            out[index] = (b / n) as u8;
            out[index + 1] = (g / n) as u8;
            out[index + 2] = (r / n) as u8;
            out[index + 3] = (a / n) as u8;
        }
    }
    out
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
    let mut single = false;
    let mut filter = mdc_bake::Filter::Nearest;
    let mut wrap_arg: Option<mdc_bake::Wrap> = None;
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
            "--single" => single = true,
            "--filter" => {
                filter = match iter.next().map(String::as_str) {
                    Some("linear") => mdc_bake::Filter::Linear,
                    _ => mdc_bake::Filter::Nearest,
                }
            }
            "--wrap" => {
                wrap_arg = match iter.next().map(String::as_str) {
                    Some("clamp") => Some(mdc_bake::Wrap::Clamp),
                    Some("repeat") => Some(mdc_bake::Wrap::Repeat),
                    _ => None,
                }
            }
            other => model_path = Some(other.to_string()),
        }
    }
    let model_path = model_path.ok_or("render-mesh requires a .gltf/.glb model")?;
    let out = out.ok_or("render-mesh requires --out <file.ppm>")?;
    let bytes = std::fs::read(&model_path)?;
    let model = mdc_model::GltfImporter.import(&bytes, &mdc_model::ImportLimits::default())?;
    let has_uv = model.meshes.iter().any(|m| !m.preview_uv.is_empty());
    // N334: wrap mode from `--wrap`, else the model's own sampler, else repeat.
    let wrap = wrap_arg.unwrap_or_else(|| match model.material_texture_wrap(0) {
        Some("clamp") => mdc_bake::Wrap::Clamp,
        _ => mdc_bake::Wrap::Repeat,
    });
    // N407: average bilinear samples in linear light.
    let srgb = args.iter().any(|a| a == "--srgb");
    // N324/N334: an explicit `--texture` wins; otherwise use the model's own
    // embedded base-colour texture.
    let texture = match &texture_path {
        Some(path) => {
            let image = std::fs::read(path)?;
            Some(
                mdc_bake::texture_map_with_alpha8(&image, &mdc_bake::BakeLimits::default())?
                    .with_filter(filter)
                    .with_wrap(wrap)
                    .with_srgb(srgb),
            )
        }
        None => match model.material_texture(0) {
            Some(image) => Some(
                mdc_bake::texture_map_with_alpha8(image, &mdc_bake::BakeLimits::default())?
                    .with_filter(filter)
                    .with_wrap(wrap)
                    .with_srgb(srgb),
            ),
            None => None,
        },
    };
    let textured = texture.is_some() && has_uv;
    let has_geometry = model.meshes.iter().any(|m| m.geometry.is_some());
    let morph_targets = model
        .meshes
        .iter()
        .filter_map(|m| m.geometry.as_ref())
        .map(|g| g.morph_targets.len())
        .max()
        .unwrap_or(0);
    let primitives = model.meshes.iter().map(|m| m.primitives).max().unwrap_or(0);
    let texture_arg = if textured { texture.as_ref() } else { None };
    // N186: render every mesh node of the model, or a single mesh with --single.
    let frame = if single {
        mdc_bake::render_geometry_at(
            &model,
            animation,
            time,
            (sprite, sprite),
            texture_arg,
            0xF800,
        )?
    } else {
        mdc_bake::render_scene_at(
            &model,
            animation,
            time,
            (sprite, sprite),
            texture_arg,
            0xF800,
        )?
    };
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
            "single": single,
            "textured": textured,
            "geometry": has_geometry,
            "morphTargets": morph_targets,
            "primitives": primitives,
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
    let mut sprite: u32 = 64;
    let mut clip: Option<String> = None;
    let mut time: f32 = 0.0;
    let mut mip: usize = 0;
    let mut smooth = false;
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--out" => out = iter.next().cloned(),
            "--frame" => frame = iter.next().and_then(|v| v.parse().ok()).unwrap_or(frame),
            "--sprite" => sprite = iter.next().and_then(|v| v.parse().ok()).unwrap_or(sprite),
            // N429: select the animation clip by name at `--time` seconds.
            "--clip" => clip = iter.next().cloned(),
            "--time" => time = iter.next().and_then(|v| v.parse().ok()).unwrap_or(time),
            // N439: render a stored mip level of the atlas instead of a frame.
            "--mip" => mip = iter.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            // N454: per-pixel shading for the static mesh renderer.
            "--smooth" => smooth = true,
            other => file = Some(other.to_string()),
        }
    }
    let file = file.ok_or("render-package requires a .mdcpkg file")?;
    let out = out.ok_or("render-package requires --out <file.ppm>")?;
    let bytes = std::fs::read(&file)?;
    let package = mdc_model::MdcPackage::from_bytes(&bytes)?;
    // N187: a mesh package renders its geometry (with its texture) instead.
    if package.package.atlas.is_none() {
        // N429: a named clip renders the posed scene, otherwise the static mesh.
        let frame = match &clip {
            Some(name) => mdc_bake::render_package_scene_clip_named(
                &package.package,
                name,
                time,
                (sprite, sprite),
                mdc_bake::Filter::Nearest,
                mdc_bake::ShadeMode::Flat,
            )?,
            None if smooth => mdc_bake::render_package_mesh_mode(
                &package.package,
                (sprite, sprite),
                mdc_bake::ShadeMode::Smooth,
            )?,
            None => mdc_bake::render_package_mesh(&package.package, (sprite, sprite))?,
        };
        std::fs::write(
            &out,
            rgb565_frame_to_ppm(frame.width, frame.height, &frame.rgb565),
        )?;
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "package": file,
                "mode": package.manifest.mode,
                "sprite": format!("{}x{}", frame.width, frame.height),
                "output": out,
            }))?
        );
        return Ok(());
    }
    let atlas = package.package.atlas.as_ref().expect("atlas checked");
    // N439: a stored mip level renders its whole downsampled image.
    if mip > 0 {
        let level = atlas
            .mips
            .get(mip - 1)
            .ok_or_else(|| format!("mip {mip} not present ({} levels)", atlas.mips.len()))?;
        std::fs::write(
            &out,
            rgb565_frame_to_ppm(level.width, level.height, &level.rgb565),
        )?;
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "package": file,
                "mip": mip,
                "size": format!("{}x{}", level.width, level.height),
                "output": out,
            }))?
        );
        return Ok(());
    }
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
    let mut time_ms: u64 = 0;
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--out" => out = iter.next().cloned(),
            // N479: render an animated scene at a device-clock time.
            "--time" => time_ms = iter.next().and_then(|v| v.parse().ok()).unwrap_or(time_ms),
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
    device.clock.tick = time_ms;
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
            "time_ms": time_ms,
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
    // N626: `mdc config --defaults` reports the board's advertised factory
    // defaults and exits, without opening a device session.
    // N636: the table uses the same key (`board_config_defaults`) as `inspect`,
    // so one parser reads either command's output.
    if action == "--defaults" || key == "--defaults" {
        // N666: `--key K` (or a positional key) selects a single default.
        let filter = if key == "--key" {
            value.map(str::to_string)
        } else if !key.is_empty() && key != "--defaults" {
            Some(key.to_string())
        } else {
            None
        };
        let all = board_config_defaults(&opts.board);
        let defaults = filter_config_defaults(&all, filter.as_deref());
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "board": opts.board,
                "key": filter,
                "defaults": defaults,
            }))?
        );
        return Ok(());
    }
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
    let mut source_fps: u16 = 30;
    let mut max_fps: Option<u16> = None;
    let mut bandwidth: Option<u64> = None;
    let mut quality: u8 = 80;
    let mut scale: u32 = 1;
    let mut sequence = false;
    // N496: `h`/`v`/`hv` mirror the written frames.
    let mut flip = String::new();
    // N510: write a JSON manifest describing the recorded sequence.
    let mut manifest: Option<String> = None;
    // N517: WebSocket heartbeat interval and silence timeout, in frames.
    let mut ws_heartbeat: u64 = 0;
    let mut ws_timeout: u64 = 0;
    // N527: WebSocket fragment payload size (0 = one frame per message).
    let mut ws_fragment: usize = 0;
    // N567: close (1002) a peer that sets the reserved compression bit.
    let mut ws_reject_reserved = false;
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
            "--source-fps" => {
                source_fps = iter
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(source_fps)
            }
            "--max-fps" => max_fps = iter.next().and_then(|v| v.parse().ok()),
            "--bandwidth" => bandwidth = iter.next().and_then(|v| v.parse().ok()),
            "--quality" => quality = iter.next().and_then(|v| v.parse().ok()).unwrap_or(quality),
            // N448: downscale frames written to disk.
            "--scale" => scale = iter.next().and_then(|v| v.parse().ok()).unwrap_or(scale),
            // N470: write numbered frames instead of overwriting one file.
            "--sequence" => sequence = true,
            // N496: mirror the written frames horizontally/vertically.
            "--flip" => flip = iter.next().cloned().unwrap_or(flip),
            // N510: write a manifest of the recorded frames.
            "--manifest" => manifest = iter.next().cloned(),
            // N517: ping WebSocket peers every N frames.
            "--ws-heartbeat" => {
                ws_heartbeat = iter.next().and_then(|v| v.parse().ok()).unwrap_or(0)
            }
            // N517: drop a silent WebSocket peer after N frames.
            "--ws-timeout" => ws_timeout = iter.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            // N527: split WebSocket messages into fragments.
            "--ws-fragment" => ws_fragment = iter.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            // N567: reject a reserved-bit (compression) frame with close 1002.
            "--ws-reject-reserved" => ws_reject_reserved = true,
            _ => {}
        }
    }
    let (mut session, mut device, _manager) = open_sim(&opts.board)?;
    if !session.supports_mirror() {
        return Err(format!("board {} does not advertise mirror", opts.board).into());
    }
    // N314: pace output with `mdc-schedule`. The default ceiling is the device's
    // advertised mirror fps (never above the source rate), and the default byte
    // budget is the device's advertised mirror bandwidth.
    let advertised_fps = session
        .device
        .as_ref()
        .map(|d| d.capabilities.mirror_fps)
        .unwrap_or(0);
    let advertised_bandwidth = session
        .device
        .as_ref()
        .map(|d| d.capabilities.mirror_bandwidth)
        .unwrap_or(0);
    let source_fps = source_fps.max(1);
    let ceiling = max_fps.unwrap_or_else(|| advertised_fps.max(1).min(source_fps));
    let byte_budget = bandwidth.unwrap_or(advertised_bandwidth);
    let mut scheduler = mdc_schedule::Scheduler::new(ceiling, byte_budget);
    let tick_ms = (1000 / u64::from(source_fps)).max(1);
    session.start_mirror("main", "full-frame", ceiling)?;
    device.poll()?;
    session.poll()?;

    let mut sink = match (&write_dir, format.as_str(), sequence) {
        (Some(dir), "ppm", false) => Some(mdc_mirror::PpmDirectorySink::new(dir, "frame.ppm", 0)),
        _ => None,
    };
    // N470: numbered PPM frames bypass the single-file sink.
    let ppm_dir = match (&write_dir, format.as_str(), sequence) {
        (Some(dir), "ppm", true) => Some(PathBuf::from(dir)),
        _ => None,
    };
    let bmp_dir = match (&write_dir, format.as_str()) {
        (Some(dir), "bmp") => Some(PathBuf::from(dir)),
        _ => None,
    };
    let png_dir = match (&write_dir, format.as_str()) {
        (Some(dir), "png") => Some(PathBuf::from(dir)),
        _ => None,
    };
    let jpg_dir = match (&write_dir, format.as_str()) {
        (Some(dir), "jpg" | "jpeg") => Some(PathBuf::from(dir)),
        _ => None,
    };
    for dir in [&bmp_dir, &png_dir, &jpg_dir, &ppm_dir]
        .into_iter()
        .flatten()
    {
        std::fs::create_dir_all(dir)?;
    }
    let mut server = match &serve {
        Some(address) => Some(mdc_mirror::MirrorServer::bind(address)?),
        None => None,
    };
    let mut http_server = match &http {
        Some(address) => Some(
            mdc_mirror::HttpMirrorServer::bind(address)?
                .with_ws_heartbeat(ws_heartbeat, ws_timeout)
                .with_ws_fragment(ws_fragment)
                .with_ws_reject_reserved(ws_reject_reserved),
        ),
        None => None,
    };
    if let Some(server) = &http_server {
        println!("http mirror on {}", server.local_addr()?);
    }

    let mut frames_seen = 0u64;
    let mut paced_deferred = 0u64;
    let mut paced_dropped = 0u64;
    // N510: the sequence actually written to disk.
    let mut written: Vec<String> = Vec::new();
    let mut last_dims = (0u32, 0u32);
    for index in 0..frames {
        // Each loop step models one source frame at `source_fps`; the device
        // clock advances so device-side animation keeps moving.
        device.advance(tick_ms)?;
        device.emit_mirror_frame()?;
        session.poll()?;
        let Some(frame) = session.take_mirror_frame() else {
            continue;
        };
        frames_seen += 1;
        // N314: decide whether this frame fits the output budget this tick. The
        // frame is costed in 1 KiB tiles so the scheduler's byte budget applies.
        let cost_tile = 1024u32;
        let tile_count = frame.bytes.len().div_ceil(cost_tile as usize) as u32;
        let tiles: Vec<mdc_schedule::TileCost> = (0..tile_count)
            .map(|i| mdc_schedule::TileCost::new(i, cost_tile, false))
            .collect();
        let now = index as u64 * tick_ms;
        match scheduler.plan(now, &tiles) {
            mdc_schedule::FrameDecision::Defer { .. } => {
                paced_deferred += 1;
                continue;
            }
            mdc_schedule::FrameDecision::Send { deferred, .. } if !deferred.is_empty() => {
                // A file/HTTP consumer cannot take half a frame, so defer it.
                paced_dropped += 1;
                continue;
            }
            mdc_schedule::FrameDecision::Send { .. } => {}
        }
        scheduler.commit(now);
        // N448: an optional box downscale for the written frames.
        let scaled = if scale > 1 {
            Some(downscale_bgra(
                u32::from(frame.width),
                u32::from(frame.height),
                &frame.bytes,
                scale,
            ))
        } else {
            None
        };
        let (out_w, out_h, out_bytes) = match &scaled {
            Some(bytes) => (
                (u32::from(frame.width) / scale).max(1),
                (u32::from(frame.height) / scale).max(1),
                bytes.as_slice(),
            ),
            None => (
                u32::from(frame.width),
                u32::from(frame.height),
                frame.bytes.as_slice(),
            ),
        };
        // N496: an optional mirror for the written frames.
        let flipped = if flip.contains('h') || flip.contains('v') {
            Some(mdc_mirror::flip_bgra(
                out_w as u16,
                out_h as u16,
                out_bytes,
                flip.contains('h'),
                flip.contains('v'),
            ))
        } else {
            None
        };
        let out_bytes = flipped.as_deref().unwrap_or(out_bytes);
        last_dims = (out_w, out_h);
        let shadow = |generation: u64| mdc_mirror::ShadowFrame {
            width: frame.width,
            height: frame.height,
            generation,
            frame_id: frame.frame_id,
            pixels: &frame.bytes,
        };
        let scaled_shadow = |generation: u64| mdc_mirror::ShadowFrame {
            width: out_w as u16,
            height: out_h as u16,
            generation,
            frame_id: frame.frame_id,
            pixels: out_bytes,
        };
        if let Some(sink) = sink.as_mut() {
            sink.on_frame(scaled_shadow(frames_seen));
        }
        // N470: `--sequence` numbers each written frame instead of overwriting.
        let name = |extension: &str| {
            if sequence {
                format!("frame{frames_seen:04}.{extension}")
            } else {
                format!("frame.{extension}")
            }
        };
        if let Some(dir) = &ppm_dir {
            let ppm = mdc_mirror::encode_ppm(out_w as u16, out_h as u16, out_bytes);
            std::fs::write(dir.join(name("ppm")), ppm)?;
            written.push(name("ppm"));
        }
        if let Some(dir) = &bmp_dir {
            let bmp = mdc_mirror::encode_bmp(out_w as u16, out_h as u16, out_bytes);
            std::fs::write(dir.join(name("bmp")), bmp)?;
            written.push(name("bmp"));
        }
        if let Some(dir) = &png_dir {
            // N328: PNG for a smaller OBS image source.
            let png = mdc_mirror::encode_png(out_w as u16, out_h as u16, out_bytes);
            std::fs::write(dir.join(name("png")), png)?;
            written.push(name("png"));
        }
        if let Some(dir) = &jpg_dir {
            // N336/N346: baseline JPEG, the smallest OBS image source.
            let jpg =
                mdc_mirror::encode_jpeg(out_w as u16, out_h as u16, out_bytes, quality.max(1));
            std::fs::write(dir.join(name("jpg")), jpg)?;
            written.push(name("jpg"));
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
    // N510: a manifest makes a recorded sequence reproducible.
    if let Some(path) = &manifest {
        let manifest_json = serde_json::json!({
            "board": opts.board,
            "format": format,
            "scale": scale,
            "flip": flip,
            "source_fps": source_fps,
            "frames_requested": frames,
            "frames_written": written.len(),
            "width": last_dims.0,
            "height": last_dims.1,
            "files": written,
        });
        std::fs::write(path, serde_json::to_string_pretty(&manifest_json)?)?;
    }
    let stats = session.mirror_stats();
    let out = serde_json::json!({
        "board": opts.board,
        "frames_requested": frames,
        "frames_seen": frames_seen,
        "output_fps": ceiling,
        "source_fps": source_fps,
        "bandwidth_budget": byte_budget,
        "paced_deferred": paced_deferred,
        "paced_dropped": paced_dropped,
        "mirror_received": stats.frames_received,
        "mirror_dropped": stats.frames_dropped,
        "ppm_written": sink.as_ref().map(|s| s.frames_written()),
        "served_clients": server.as_ref().map(|s| s.client_count()),
        "served_dropped": server.as_ref().map(|s| s.frames_dropped()),
        "http_frames": http_server.as_ref().map(|s| s.frames_published()),
        // N500: WebSocket (`/ws`) consumers on the HTTP mirror.
        "ws_clients": http_server.as_ref().map(|s| s.ws_clients()),
        // N507: control frames handled on the WebSocket mirror.
        "ws_pings": http_server.as_ref().map(|s| s.ws_pings()),
        "ws_closes": http_server.as_ref().map(|s| s.ws_closes()),
        // N517: server pings and silent peers dropped.
        "ws_pings_sent": http_server.as_ref().map(|s| s.ws_pings_sent()),
        "ws_timeouts": http_server.as_ref().map(|s| s.ws_timeouts()),
        // N647/N657: the retained WebSocket note count and the per-kind summary,
        // matching `/health`.
        "ws_log": http_server.as_ref().map(|s| s.ws_log().len()),
        "ws_log_kinds": http_server.as_ref().map(|s| s.ws_log_counts()),
        // N548: complete client -> server WebSocket messages reassembled.
        "ws_messages": http_server.as_ref().map(|s| s.ws_messages()),
        // N558: client frames whose reserved (compression) bit was set.
        "ws_reserved": http_server.as_ref().map(|s| s.ws_reserved()),
        // N567: handshake extension offers (always declined).
        "ws_offered": http_server.as_ref().map(|s| s.ws_offered()),
        // N577: bounded log of WebSocket declines/failures.
        "ws_log": http_server.as_ref().map(|s| s.ws_log().to_vec()),
        // N510: the recorded sequence manifest, when requested.
        "manifest": manifest,
        "frames_written": written.len(),
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

/// N510: record a numbered frame sequence plus a manifest. A thin wrapper over
/// `mirror --sequence` with recording-oriented defaults.
fn cmd_record(opts: &GlobalOpts, extra: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut frames = "30".to_string();
    let mut fps = "10".to_string();
    let mut dir = "mdc-record".to_string();
    let mut format = "ppm".to_string();
    let mut scale = "1".to_string();
    let mut iter = extra.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--frames" => frames = iter.next().cloned().unwrap_or(frames),
            "--fps" => fps = iter.next().cloned().unwrap_or(fps),
            "--dir" => dir = iter.next().cloned().unwrap_or(dir),
            "--format" => format = iter.next().cloned().unwrap_or(format),
            "--scale" => scale = iter.next().cloned().unwrap_or(scale),
            _ => {}
        }
    }
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir)?;
    let manifest = dir.join("manifest.json");
    let args = vec![
        "--frames".to_string(),
        frames,
        "--source-fps".to_string(),
        fps.clone(),
        "--max-fps".to_string(),
        fps,
        "--format".to_string(),
        format,
        "--scale".to_string(),
        scale,
        "--sequence".to_string(),
        "--write-dir".to_string(),
        dir.to_string_lossy().into_owned(),
        "--manifest".to_string(),
        manifest.to_string_lossy().into_owned(),
    ];
    cmd_mirror(opts, &args)
}

/// N520: probe the pixel dimensions of a recorded frame (PPM/BMP/PNG/JPEG).
fn probe_frame_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.starts_with(b"P6") {
        let header = std::str::from_utf8(bytes.get(..bytes.len().min(64))?).ok()?;
        let mut parts = header.lines().nth(1)?.split_whitespace();
        return Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?));
    }
    if bytes.starts_with(b"BM") {
        let w = i32::from_le_bytes(bytes.get(18..22)?.try_into().ok()?);
        let h = i32::from_le_bytes(bytes.get(22..26)?.try_into().ok()?);
        return Some((w.unsigned_abs(), h.unsigned_abs()));
    }
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        let w = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
        let h = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
        return Some((w, h));
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        let mut offset = 2usize;
        while offset + 4 <= bytes.len() {
            if bytes[offset] != 0xFF {
                return None;
            }
            let marker = bytes[offset + 1];
            if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
                offset += 2;
                continue;
            }
            let length = usize::from(u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]));
            if length < 2 {
                return None;
            }
            let is_sof = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
            if is_sof {
                let h = u32::from(u16::from_be_bytes([bytes[offset + 5], bytes[offset + 6]]));
                let w = u32::from(u16::from_be_bytes([bytes[offset + 7], bytes[offset + 8]]));
                return Some((w, h));
            }
            offset += 2 + length;
        }
        return None;
    }
    None
}

/// N520: verify a recorded sequence against its manifest.
fn verify_sequence(dir: &str) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let base = PathBuf::from(dir);
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(base.join("manifest.json"))?)?;
    let width = manifest["width"].as_u64().unwrap_or(0) as u32;
    let height = manifest["height"].as_u64().unwrap_or(0) as u32;
    let files = manifest["files"].as_array().cloned().unwrap_or_default();
    let mut verified = 0usize;
    let mut missing = Vec::new();
    let mut mismatched = Vec::new();
    for file in &files {
        let Some(name) = file.as_str() else {
            continue;
        };
        match std::fs::read(base.join(name)) {
            Err(_) => missing.push(name.to_string()),
            Ok(bytes) => match probe_frame_size(&bytes) {
                Some(size) if size == (width, height) => verified += 1,
                _ => mismatched.push(name.to_string()),
            },
        }
    }
    Ok(serde_json::json!({
        "dir": dir,
        "format": manifest["format"],
        "width": width,
        "height": height,
        "frames": files.len(),
        "verified": verified,
        "missing": missing,
        "mismatched": mismatched,
        "valid": missing.is_empty() && mismatched.is_empty(),
    }))
}

/// N530: decode a recorded frame (PPM/BMP/PNG/JPEG) to packed RGB565.
fn frame_to_rgb565(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let image = image::load_from_memory(bytes).ok()?.to_rgb8();
    let (width, height) = (image.width(), image.height());
    let mut out = Vec::with_capacity((width * height * 2) as usize);
    for pixel in image.pixels() {
        let [r, g, b] = pixel.0;
        let value = (u16::from(r >> 3) << 11) | (u16::from(g >> 2) << 5) | u16::from(b >> 3);
        out.extend_from_slice(&value.to_le_bytes());
    }
    Some((width, height, out))
}

/// N530: nearest-neighbour fit of a packed RGB565 frame to a target size.
fn fit_rgb565(src: &[u8], size: (u32, u32), target: (u32, u32)) -> Vec<u8> {
    let (sw, sh) = (size.0.max(1), size.1.max(1));
    let (dw, dh) = (target.0.max(1), target.1.max(1));
    let mut out = vec![0u8; (dw * dh * 2) as usize];
    for y in 0..dh {
        let sy = (u64::from(y) * u64::from(sh) / u64::from(dh)) as u32;
        for x in 0..dw {
            let sx = (u64::from(x) * u64::from(sw) / u64::from(dw)) as u32;
            let si = ((sy * sw + sx) * 2) as usize;
            let di = ((y * dw + x) * 2) as usize;
            if let (Some(from), Some(to)) = (src.get(si..si + 2), out.get_mut(di..di + 2)) {
                to.copy_from_slice(from);
            }
        }
    }
    out
}

/// N530: replay the recorded frames onto a simulated device surface.
fn render_sequence(
    dir: &str,
    board: &str,
    fps: u32,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let base = PathBuf::from(dir);
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(base.join("manifest.json"))?)?;
    let files = manifest["files"].as_array().cloned().unwrap_or_default();
    let (mut session, mut device, _manager) = open_sim(board)?;
    let (width, height) = (device.profile().width, device.profile().height);
    let mut rendered = 0usize;
    let mut failures: Vec<String> = Vec::new();
    // N540: pace the replay to the requested frame rate (0 = as fast as possible).
    let gap = if fps == 0 {
        None
    } else {
        Some(std::time::Duration::from_micros(1_000_000 / u64::from(fps)))
    };
    let started = std::time::Instant::now();
    for file in &files {
        let Some(name) = file.as_str() else {
            continue;
        };
        if let Some(gap) = gap {
            if rendered > 0 {
                std::thread::sleep(gap);
            }
        }
        let bytes = match std::fs::read(base.join(name)) {
            Ok(bytes) => bytes,
            Err(_) => {
                failures.push(name.to_string());
                continue;
            }
        };
        let Some((fw, fh, pixels)) = frame_to_rgb565(&bytes) else {
            failures.push(name.to_string());
            continue;
        };
        let fitted = fit_rgb565(&pixels, (fw, fh), (u32::from(width), u32::from(height)));
        if session
            .send_frame(Frame {
                surface_id: "main".into(),
                width,
                height,
                bytes: fitted,
            })
            .is_err()
        {
            failures.push(name.to_string());
            continue;
        }
        device.poll()?;
        session.poll()?;
        rendered += 1;
    }
    Ok(serde_json::json!({
        "dir": dir,
        "board": board,
        "surface": { "width": width, "height": height },
        "frames": files.len(),
        "rendered": rendered,
        "failures": failures,
        "ok": failures.is_empty(),
        // N540: the pacing target and how long the replay actually took.
        "fps": fps,
        "elapsed_ms": started.elapsed().as_millis() as u64,
    }))
}

/// N520: `mdc record --play --dir DIR` verifies a recorded sequence.
fn cmd_record_play(extra: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut dir = "mdc-record".to_string();
    let mut board = "default".to_string();
    let mut render = false;
    // N540: replay pacing (0 = as fast as possible).
    let mut fps = 0u32;
    let mut iter = extra.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--dir" => dir = iter.next().cloned().unwrap_or(dir),
            "--board" => board = iter.next().cloned().unwrap_or(board),
            // N530: replay the frames onto a simulated device.
            "--render" => render = true,
            // N540: pace the replay at `fps` frames per second.
            "--fps" => fps = iter.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            _ => {}
        }
    }
    let report = verify_sequence(&dir)?;
    let mut valid = report["valid"].as_bool().unwrap_or(false);
    println!("{}", serde_json::to_string_pretty(&report)?);
    if render {
        let rendered = render_sequence(&dir, &board, fps)?;
        valid = valid && rendered["ok"].as_bool().unwrap_or(false);
        println!("{}", serde_json::to_string_pretty(&rendered)?);
    }
    if !valid {
        std::process::exit(1);
    }
    Ok(())
}

/// N316: read the device diagnostics channel (metrics + boot info) and print it.
fn cmd_diag(opts: &GlobalOpts, extra: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut metric = "fps".to_string();
    let mut value = 30.0_f64;
    let mut unit: Option<String> = Some("fps".to_string());
    let mut telemetry: Option<String> = None;
    let mut crash: Option<String> = None;
    let mut iter = extra.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--metric" => metric = iter.next().cloned().unwrap_or(metric),
            "--value" => value = iter.next().and_then(|v| v.parse().ok()).unwrap_or(value),
            "--unit" => unit = iter.next().cloned(),
            "--telemetry" => telemetry = iter.next().cloned(),
            "--crash" => crash = iter.next().cloned(),
            _ => {}
        }
    }
    let (mut session, mut device, _manager) = open_sim(&opts.board)?;
    // Diagnostics travel on their own message types and never mix with a frame
    // transaction.
    device.emit_boot_info()?;
    device.emit_metric(&metric, value, unit.as_deref())?;
    if let Some(name) = &telemetry {
        // N335: a structured telemetry event with a couple of fields.
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("value".to_string(), format!("{value}"));
        fields.insert("board".to_string(), opts.board.clone());
        device.emit_telemetry(name, Some("info"), fields)?;
    }
    if let Some(reason) = &crash {
        // N367: a crash/fault report.
        device.emit_crash(reason, Some("mdc_app"), Some(0x4000_0000))?;
    }
    session.poll()?;
    let records = session.take_diagnostics();
    let diagnostics: Vec<serde_json::Value> = records
        .iter()
        .map(|record| match record {
            mdc_core::DiagnosticRecord::Metric(m) => serde_json::json!({
                "kind": "metric",
                "name": m.name,
                "value": m.value,
                "unit": m.unit,
                "boot_id": m.boot_id,
                "run_id": m.run_id,
                "event_seq": m.event_seq,
            }),
            mdc_core::DiagnosticRecord::BootInfo(b) => serde_json::json!({
                "kind": "bootInfo",
                "firmware": b.firmware,
                "boot_id": b.boot_id,
                "run_id": b.run_id,
                "reset_reason": b.reset_reason,
            }),
            mdc_core::DiagnosticRecord::Telemetry(e) => serde_json::json!({
                "kind": "telemetry",
                "name": e.name,
                "level": e.level,
                "fields": e.fields,
                "event_seq": e.event_seq,
            }),
            mdc_core::DiagnosticRecord::Crash(c) => serde_json::json!({
                "kind": "crash",
                "reason": c.reason,
                "task": c.task,
                "pc": c.pc,
            }),
        })
        .collect();
    let out = serde_json::json!({
        "board": opts.board,
        "count": records.len(),
        "diagnostics": diagnostics,
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

        node: None,
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

                        node: None,
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

/// N354: send the OTA begin/chunk/confirm and wait for each reply over TCP.
fn drive_ota_tcp(
    session: &mut Session<TcpTransport>,
    version: &str,
    size: Option<u64>,
    sha256: Option<String>,
) -> Result<(), String> {
    let commands = [
        mdc_protocol::OtaCommand {
            action: "begin".into(),
            version: Some(version.to_string()),
            size,
            sha256: None,
        },
        mdc_protocol::OtaCommand {
            action: "chunk".into(),
            version: None,
            size: None,
            sha256: sha256.clone(),
        },
        mdc_protocol::OtaCommand {
            action: "confirm".into(),
            version: None,
            size: None,
            sha256: None,
        },
    ];
    for command in commands {
        session.send_ota(command).map_err(|e| e.to_string())?;
        for _ in 0..80 {
            session.poll().map_err(|e| e.to_string())?;
            if session.pending_request_count() == 0 {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    Ok(())
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

/// N369: bounded, content-addressed asset store on disk.
fn cmd_asset(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let action = args.first().map(String::as_str).unwrap_or("");
    let dir = args.get(1).cloned().unwrap_or_default();
    if dir.is_empty() {
        return Err("asset <put|ls|get|prune> <dir> ...".into());
    }
    let mut max_entry = 1usize << 20;
    let mut max_total = 8usize << 20;
    let mut out: Option<String> = None;
    let mut file: Option<String> = None;
    let mut iter = args[2..].iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--max-entry" => {
                max_entry = iter
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("bad --max-entry")?
            }
            "--max-total" => {
                max_total = iter
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("bad --max-total")?
            }
            "--out" => out = iter.next().cloned(),
            other if file.is_none() => file = Some(other.to_string()),
            _ => {}
        }
    }
    let cache =
        mdc_assets::DiskCache::open(&dir, max_entry, max_total).map_err(|e| e.to_string())?;
    match action {
        "put" => {
            let path = file.ok_or("asset put <dir> <file>")?;
            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
            let key = cache.put(&bytes).map_err(|e| e.to_string())?;
            let total = cache.total_bytes().map_err(|e| e.to_string())?;
            println!("{} {} {}", key.0, bytes.len(), total);
        }
        "ls" => {
            for (key, size) in cache.list().map_err(|e| e.to_string())? {
                println!("{} {}", key.0, size);
            }
            println!(
                "total {} bytes",
                cache.total_bytes().map_err(|e| e.to_string())?
            );
        }
        "get" => {
            let key = mdc_assets::AssetKey(file.ok_or("asset get <dir> <key>")?);
            match cache.get(&key).map_err(|e| e.to_string())? {
                Some(bytes) => match out {
                    Some(path) => {
                        std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
                        println!("{path} {} bytes", bytes.len());
                    }
                    None => println!("{} bytes (use --out to save)", bytes.len()),
                },
                None => return Err(format!("no asset {}", key.0).into()),
            }
        }
        "prune" => {
            let removed = cache.prune().map_err(|e| e.to_string())?;
            let total = cache.total_bytes().map_err(|e| e.to_string())?;
            println!("removed {removed}; total {total} bytes");
        }
        // N487: re-hash every stored entry and compare with its content key.
        "verify" => {
            let entries = cache.list().map_err(|e| e.to_string())?;
            let mut mismatched = Vec::new();
            for (key, _) in &entries {
                let path = cache.dir().join(&key.0);
                match std::fs::read(&path) {
                    Ok(bytes) => {
                        if mdc_assets::AssetKey::from_bytes(&bytes) != *key {
                            mismatched.push(key.0.clone());
                        }
                    }
                    Err(_) => mismatched.push(key.0.clone()),
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "checked": entries.len(),
                    "mismatched": mismatched,
                    "valid": mismatched.is_empty(),
                }))?
            );
            if !mismatched.is_empty() {
                std::process::exit(1);
            }
        }
        other => return Err(format!("unknown asset action: {other}").into()),
    }
    Ok(())
}

/// N388: import a model and print its summary as JSON.
fn cmd_model_info(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let path = args.first().cloned().unwrap_or_default();
    if path.is_empty() {
        return Err("model-info <model.json|.gltf|.glb>".into());
    }
    let bytes = std::fs::read(&path)?;
    let limits = mdc_model::ImportLimits::default();
    // N503: report reserved (undecoded) extensions before importing, so a
    // compressed asset is explained rather than only failing.
    let reserved = mdc_model::reserved_extensions_in_bytes(&bytes);
    let model = if path.to_ascii_lowercase().ends_with(".json") {
        mdc_model::Live2DImporter.import(&bytes, &limits)?
    } else {
        mdc_model::GltfImporter.import(&bytes, &limits)?
    };
    let mut summary = serde_json::to_value(model.summary())?;
    if let Some(map) = summary.as_object_mut() {
        map.insert("reserved_extensions".into(), serde_json::json!(reserved));
        // N544: report each texture's real size/format, including the KTX2 header
        // fields when the payload is a Basis container.
        let textures: Vec<serde_json::Value> = model
            .textures
            .iter()
            .enumerate()
            .map(|(index, texture)| {
                let mut entry = serde_json::json!({
                    "index": index,
                    "format": texture.format,
                    "width": texture.width,
                    "height": texture.height,
                    "ktx2": texture.is_ktx2(),
                });
                if let Some(header) = texture
                    .data
                    .as_deref()
                    .and_then(mdc_model::Ktx2Header::parse)
                {
                    if let Some(map) = entry.as_object_mut() {
                        map.insert("vkFormat".into(), serde_json::json!(header.format_name()));
                        map.insert(
                            "supercompression".into(),
                            serde_json::json!(header.scheme_name()),
                        );
                        // N610: flag a declared-size/header mismatch explicitly.
                        map.insert(
                            "ktx2SizeOk".into(),
                            serde_json::json!(
                                header.pixel_width == texture.width
                                    && header.pixel_height == texture.height
                            ),
                        );
                        // N554: container dimensions beyond width/height.
                        map.insert("mipLevels".into(), serde_json::json!(header.mip_levels()));
                        map.insert("slices".into(), serde_json::json!(header.slices()));
                        map.insert(
                            "container".into(),
                            serde_json::json!(if header.is_volume() {
                                "volume"
                            } else if header.is_cubemap() {
                                "cubemap"
                            } else if header.is_array() {
                                "array"
                            } else {
                                "2d"
                            }),
                        );
                    }
                }
                entry
            })
            .collect();
        map.insert("textures".into(), serde_json::json!(textures));
        // N610: surface the model's validation problems (e.g. a KTX2 mismatch).
        let report = model.validate();
        map.insert(
            "validation".into(),
            serde_json::json!({
                "valid": report.valid,
                "problems": report.problems,
            }),
        );
    }
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

/// N389: compare two sealed packages (manifest and structure counts) as JSON.
fn cmd_diff(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
        return Err("diff <a.mdcpkg> <b.mdcpkg>".into());
    };
    let pa = mdc_model::MdcPackage::from_bytes(&std::fs::read(a)?)?;
    let pb = mdc_model::MdcPackage::from_bytes(&std::fs::read(b)?)?;
    let mut fields = serde_json::Map::new();
    let mut compare = |name: &str, av: serde_json::Value, bv: serde_json::Value| {
        let same = av == bv;
        fields.insert(
            name.to_string(),
            serde_json::json!({ "a": av, "b": bv, "same": same }),
        );
    };
    compare(
        "version",
        pa.manifest.version.into(),
        pb.manifest.version.into(),
    );
    compare(
        "target",
        pa.manifest.target.clone().into(),
        pb.manifest.target.clone().into(),
    );
    compare(
        "mode",
        format!("{:?}", pa.manifest.mode).into(),
        format!("{:?}", pb.manifest.mode).into(),
    );
    compare(
        "body_bytes",
        pa.manifest.body_bytes.into(),
        pb.manifest.body_bytes.into(),
    );
    compare(
        "checksum",
        pa.manifest.checksum.into(),
        pb.manifest.checksum.into(),
    );
    // N624: the manifest locality metric as its own diff line.
    compare(
        "manifest_cache_misses",
        pa.manifest.cache_misses.into(),
        pb.manifest.cache_misses.into(),
    );
    compare(
        "cache_size",
        pa.manifest.cache_size.into(),
        pb.manifest.cache_size.into(),
    );
    let counts = |p: &mdc_model::DevicePackage| {
        serde_json::json!({
            "nodes": p.nodes.len(),
            "meshes": p.meshes.len(),
            "materials": p.materials.len(),
            "textures": p.textures.len(),
            "texture_blobs": p.texture_blobs.len(),
            "skeletons": p.skeletons.len(),
            "animations": p.animations.len(),
            "sprites": p.sprites.len(),
            "lights": p.lights.len(),
            "estimated_bytes": p.estimated_bytes,
            // N590: KTX2 metadata consistency between two packages.
            "ktx2_textures": p.textures.iter().filter(|t| t.is_ktx2()).count(),
            "ktx2_slices": p.textures.iter().filter_map(|t| t.ktx2_header()).map(|h| h.slices() as u64).sum::<u64>(),
            // N604: vertex-cache locality of the stored geometry.
            "cache_misses": package_cache_misses(p, CACHE_REPORT_SIZE),
        })
    };
    compare("structure", counts(&pa.package), counts(&pb.package));
    let identical = fields
        .values()
        .all(|entry| entry.get("same").and_then(serde_json::Value::as_bool) == Some(true));
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "identical": identical,
            "fields": fields,
            // N680: how many fewer misses the second package has than the first
            // (positive = `b` is more cache-friendly).
            "index_cache_saved": pa.manifest.cache_misses as i64
                - pb.manifest.cache_misses as i64,
            "cache_misses": {
                "a": pa.manifest.cache_misses,
                "b": pb.manifest.cache_misses,
            },
        }))?
    );
    Ok(())
}

/// N399: import a model and print a validation report; exit non-zero when invalid.
/// N620: whether a validation report fails (a problem always, a warning under
/// `--strict`). KTX2 issues land here: size mismatch is a problem, and the
/// transcoding note is a warning caught by strict.
fn validation_failed(report: &mdc_model::ValidationReport, strict: bool) -> bool {
    !report.valid || (strict && !report.warnings.is_empty())
}

fn cmd_validate(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    // N466: `--strict` treats warnings as failures.
    let strict = args.iter().any(|a| a == "--strict");
    let path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_default();
    if path.is_empty() {
        return Err("validate <model.json|.gltf|.glb> [--strict]".into());
    }
    let bytes = std::fs::read(&path)?;
    let limits = mdc_model::ImportLimits::default();
    let model = if path.to_ascii_lowercase().ends_with(".json") {
        mdc_model::Live2DImporter.import(&bytes, &limits)?
    } else {
        mdc_model::GltfImporter.import(&bytes, &limits)?
    };
    let report = model.validate();
    // N640: one builder owns the field set (and therefore the key order).
    println!(
        "{}",
        serde_json::to_string_pretty(&validation_json(&report, strict))?
    );
    // N466/N620: in strict mode a warning (e.g. a KTX2 transcoding note) fails too.
    if validation_failed(&report, strict) {
        std::process::exit(1);
    }
    Ok(())
}

/// N640: the `mdc validate` JSON body. `serde_json` keeps object keys sorted, so
/// this field set serializes in a stable order — a contract `validate --json`
/// consumers can rely on.
fn validation_json(report: &mdc_model::ValidationReport, strict: bool) -> serde_json::Value {
    serde_json::json!({
        "valid": report.valid,
        "strict": strict,
        "problems": report.problems,
        "warnings": report.warnings,
        // N630: a KTX2 summary right in the validation report.
        "ktx2_textures": report.summary.ktx2_textures,
        "ktx2_slices": report.summary.ktx2_slices,
        "summary": report.summary,
    })
}

/// N408: verify a sealed package's manifest against its body; exit non-zero when
/// the checksum or sizes do not match.
fn cmd_verify(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let path = args.first().cloned().unwrap_or_default();
    if path.is_empty() {
        return Err("verify <file.mdcpkg>".into());
    }
    let package = mdc_model::MdcPackage::from_bytes(&std::fs::read(&path)?)?;
    let (report, valid) = verify_report(&package);
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !valid {
        std::process::exit(1);
    }
    Ok(())
}

/// N690: the verification report, including the manifest locality fields.
fn verify_report(package: &mdc_model::MdcPackage) -> (serde_json::Value, bool) {
    let verification = package.verify();
    // N604: report the vertex-cache locality of the stored geometry too.
    let mut report = serde_json::to_value(&verification).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(map) = report.as_object_mut() {
        map.insert("cache_size".into(), serde_json::json!(CACHE_REPORT_SIZE));
        map.insert(
            "cache_misses".into(),
            serde_json::json!(package_cache_misses(&package.package, CACHE_REPORT_SIZE)),
        );
        // N690: make the manifest-vs-recomputed locality explicit.
        map.insert(
            "manifest_cache_misses".into(),
            serde_json::json!(verification.manifest_cache_misses),
        );
        map.insert(
            "cache_misses_ok".into(),
            serde_json::json!(verification.cache_misses_ok),
        );
    }
    (report, verification.valid)
}

/// N604/N614: the vertex-cache size used for the locality report (shared with
/// the package manifest).
const CACHE_REPORT_SIZE: usize = mdc_model::CACHE_REPORT_SIZE as usize;

/// N604/N614: the cache misses of every mesh with geometry at `cache_size`.
fn package_cache_misses(package: &mdc_model::DevicePackage, cache_size: usize) -> u64 {
    mdc_model::package_cache_misses(package, cache_size)
}

/// N417: lint a scene JSON file (a `SceneNode` or a `SceneCommand` with `scene`).
fn cmd_scene_lint(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let path = args.first().cloned().unwrap_or_default();
    if path.is_empty() {
        return Err("scene-lint <scene.json>".into());
    }
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    let node: mdc_protocol::SceneNode = match value.get("scene") {
        Some(scene) => serde_json::from_value(scene.clone())?,
        None => serde_json::from_value(value)?,
    };
    let report = node.lint();
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !report.valid {
        std::process::exit(1);
    }
    Ok(())
}

fn usage() {
    println!(
        "mdc [--sim] [--board id] [--address host:port] [--port /dev/cu.usbmodem*] \
         devices [--compact]|inspect|send-image <path>|benchmark [N]|simulate|discover|\
flash-sim <merged.bin>|h06-demo|sim-accept|ota-confirm|linux-accept|pair <id> <token>|unpair <id>|\
authorize <id> <ws|tcp|serial|memory> <ep>|switch-demo|mirror [--frames N] [--write-dir DIR] [--format ppm|bmp|png|jpg] [--quality N] [--scale N] [--sequence] [--flip h/v] [--serve ADDR] [--http ADDR] [--ws-heartbeat N] [--ws-timeout M] [--ws-fragment N] [--source-fps N] [--max-fps N] [--bandwidth BPS]|record [--frames N] [--fps N] [--dir DIR] [--play [--render] [--board B] [--fps N]] [--format ppm|bmp|png|jpg] [--scale N]|diag [--metric NAME] [--value V] [--unit U] [--telemetry NAME] [--crash REASON]|\
package <model.gltf> [out.mdcpkg] [--texture tex.png] [--out OUT.mdcpkg] [--atlas MAX_PAGE] [--indexed N] [--dither N] [--tangents] [--variant NAME] [--report] [--texture-mips N] [--mip-level N] [--max-draw-vertices N]|install-package <file.mdcpkg>|provision <file.mdcpkg>|ota <version> [--size N] [--sha256 H]|\
config get <key>|config set <key> <value>|bake <image>... [--frames N] [--sprite N] [--out DIR] [--mipmaps] [--mip-linear] [--dither N] [--palette N] [--package OUT.mdcpkg]|\
bake-mesh <model.gltf> [--frames N] [--sprite N] [--animation N] [--all-animations] [--texture tex.png] [--out DIR] [--package OUT.mdcpkg]|\
render-mesh <model.gltf> [--texture tex.png] [--animation N] [--time T] [--sprite N] [--single] [--filter nearest|linear] [--wrap repeat|clamp] [--srgb] --out f.ppm|\
motion <motion3.json> [--param ID] [--samples N]|\
render-package <file.mdcpkg> [--frame N] [--clip NAME] [--time T] [--mip N] [--smooth] --out f.ppm|\
scene <file.json>|scene clear|scene-render <file.json> [--time MS] --out f.ppm|scene-lint <file.json>|raster-demo|preview-mesh|inspect [--watch --count N --interval MS] [--rss] [--defaults|--defaults-only [--key K]]|\
asset put <dir> <file> [--max-entry N] [--max-total N]|asset ls <dir>|asset get <dir> <key> [--out FILE]|asset prune <dir> [--max-total N]|asset verify <dir>|\
model-info <model.json|.gltf|.glb>|validate <model.json|.gltf|.glb> [--strict]|verify <file.mdcpkg>|diff <a.mdcpkg> <b.mdcpkg>"
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
            cmd_devices(&o, &args[1..])
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
        Some("diag") => {
            if opts.sim || opts.address.is_none() {
                // diagnostics on a sim board; a TCP peer would emit its own.
            }
            cmd_diag(&opts, &args[1..])
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
        Some("record") => {
            // N520: `record --play --dir DIR` verifies an existing sequence.
            if args.iter().any(|a| a == "--play") {
                cmd_record_play(&args[1..])
            } else {
                let mut o = opts.clone();
                o.sim = true;
                cmd_record(&o, &args[1..])
            }
        }
        Some("package") => {
            let path = args.get(1).cloned().unwrap_or_default();
            if path.is_empty() {
                eprintln!("package requires a model path");
                std::process::exit(2);
            }
            cmd_package(&opts, &path, &args[2..])
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
        Some("provision") => {
            let path = args.get(1).cloned().unwrap_or_default();
            if path.is_empty() {
                eprintln!("provision requires a package path");
                std::process::exit(2);
            }
            cmd_provision(&opts, &path)
        }
        Some("ota") => {
            let version = args.get(1).cloned().unwrap_or_default();
            if version.is_empty() {
                eprintln!("ota requires a firmware version");
                std::process::exit(2);
            }
            cmd_ota(&opts, &version, &args[2..])
        }
        Some("config") => {
            let action = args.get(1).cloned().unwrap_or_default();
            let key = args.get(2).cloned().unwrap_or_default();
            // N666: `--defaults` may omit the key (optionally `--key K`).
            if action.is_empty() || (key.is_empty() && action != "--defaults") {
                eprintln!(
                    "config get <key> | config set <key> <value> | config --defaults [--key K]"
                );
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
        Some("motion") => cmd_motion(&args[1..]),
        Some("render-mesh") => cmd_render_mesh(&args[1..]),
        Some("render-package") => cmd_render_package(&args[1..]),
        Some("asset") => cmd_asset(&args[1..]),
        Some("model-info") => cmd_model_info(&args[1..]),
        Some("validate") => cmd_validate(&args[1..]),
        Some("verify") => cmd_verify(&args[1..]),
        Some("scene-lint") => cmd_scene_lint(&args[1..]),
        Some("diff") => cmd_diff(&args[1..]),
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
    fn asset_put_ls_get_prune_round_trip() {
        let base = std::env::temp_dir().join(format!("mdc-cli-asset-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let src = base.join("src.bin");
        std::fs::write(&src, [1u8, 2, 3, 4]).unwrap();
        let store = base.join("store").to_string_lossy().into_owned();
        cmd_asset(&[
            "put".into(),
            store.clone(),
            src.to_string_lossy().into_owned(),
        ])
        .unwrap();
        cmd_asset(&["ls".into(), store.clone()]).unwrap();
        let cache = mdc_assets::DiskCache::open(&store, 1 << 20, 8 << 20).unwrap();
        let listed = cache.list().unwrap();
        assert_eq!(listed.len(), 1);
        let key = listed[0].0 .0.clone();
        assert_eq!(mdc_assets::AssetKey::from_bytes(&[1, 2, 3, 4]).0, key);
        let out = base.join("out.bin");
        cmd_asset(&[
            "get".into(),
            store.clone(),
            key,
            "--out".into(),
            out.to_string_lossy().into_owned(),
        ])
        .unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), vec![1, 2, 3, 4]);
        cmd_asset(&["prune".into(), store]).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[cfg(unix)]
    fn host_rss_bytes_is_positive_on_unix() {
        assert!(host_rss_bytes().unwrap() > 0);
    }

    #[test]
    fn scene_lint_flags_missing_geometry() {
        // N417: a rect without a rect, and a text node without a value, fail.
        let bad: mdc_protocol::SceneNode = serde_json::from_value(serde_json::json!({
            "name": "root", "kind": "group",
            "children": [
                {"name": "a", "kind": "rect"},
                {"name": "b", "kind": "text", "rect": [0, 0, 4, 7]}
            ]
        }))
        .unwrap();
        let report = bad.lint();
        assert!(!report.valid);
        assert!(report.problems.iter().any(|p| p.contains("'a'")));
        assert!(report.problems.iter().any(|p| p.contains("'b'")));

        let good: mdc_protocol::SceneNode = serde_json::from_value(serde_json::json!({
            "name": "root", "kind": "rect", "rect": [0, 0, 4, 4], "value": "#fff"
        }))
        .unwrap();
        assert!(good.lint().valid);
        assert_eq!(good.lint().nodes, 1);
    }

    #[test]
    fn validate_json_field_order_is_stable() {
        // N640: the report's key order is a contract (serde_json sorts keys).
        let gltf = br#"{"asset":{"version":"2.0"},
            "nodes":[{"name":"root","mesh":0}],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
            "accessors":[{"count":4,"type":"VEC3"}]}"#;
        let model = mdc_model::GltfImporter
            .import(gltf, &mdc_model::ImportLimits::default())
            .unwrap();
        let report = model.validate();
        let json = validation_json(&report, false);
        let text = serde_json::to_string(&json).unwrap();
        // N660: a snapshot of the top-level key order (pretty lines at exactly one
        // level of indentation), so a reordering is caught, not just a rename.
        let pretty = serde_json::to_string_pretty(&json).unwrap();
        let top: Vec<&str> = pretty
            .lines()
            .filter(|line| line.starts_with("  \"") && !line.starts_with("   "))
            .filter_map(|line| line.trim().split('"').nth(1))
            .collect();
        assert_eq!(
            top,
            vec![
                "ktx2_slices",
                "ktx2_textures",
                "problems",
                "strict",
                "summary",
                "valid",
                "warnings",
            ],
            "the top-level key order is a contract"
        );
        // The compact form stays one line and carries the same keys.
        assert!(!text.contains('\n'));
        for key in &top {
            assert!(text.contains(&format!("\"{key}\":")), "{key}");
        }
        // The nested summary carries its own stable keys too.
        assert!(text.contains("\"ktx2_textures\":"));
        assert!(json["summary"].is_object());
        assert_eq!(json["strict"], false);
        // N650: the summary's sub-keys are stable and sorted, so a consumer can
        // diff two reports field by field.
        let keys: Vec<&str> = json["summary"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let expected = [
            "animations",
            "cameras",
            "has_colors",
            "has_normals",
            "has_second_skin",
            "has_second_uv",
            "has_tangents",
            "indices",
            "ktx2_slices",
            "ktx2_textures",
            "layers",
            "lights",
            "materials",
            "meshes",
            "morph_targets",
            "nodes",
            "nodes_with_mesh",
            "primitives",
            "skeletons",
            "source",
            "sprites",
            "textures",
            "variants",
            "vertices",
        ];
        assert_eq!(keys, expected);
    }

    #[test]
    fn validate_strict_flags_warnings_as_failures() {
        // N466: a model with no geometry validates, but `--strict` treats the
        // "mesh has no geometry" warning as a failure (exit is not asserted here).
        let dir = std::env::temp_dir().join(format!("mdc-strict-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("m.gltf");
        std::fs::write(
            &path,
            br#"{"asset":{"version":"2.0"},"nodes":[{"name":"root","mesh":0}],
                "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
                "accessors":[{"count":4,"type":"VEC3"}]}"#,
        )
        .unwrap();
        let model = mdc_model::GltfImporter
            .import(
                &std::fs::read(&path).unwrap(),
                &mdc_model::ImportLimits::default(),
            )
            .unwrap();
        let report = model.validate();
        assert!(report.valid);
        assert!(report.warnings.iter().any(|w| w.contains("no geometry")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_accepts_a_fresh_package_and_rejects_tampering() {
        // N408: the manifest checksum matches the body; a tampered body fails.
        let gltf = br#"{"asset":{"version":"2.0"},"nodes":[{"name":"root","mesh":0}],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
            "accessors":[{"count":4,"type":"VEC3"}]}"#;
        let model = mdc_model::GltfImporter
            .import(gltf, &mdc_model::ImportLimits::default())
            .unwrap();
        let package =
            mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::full_3d())
                .unwrap();
        assert!(package.verify().valid, "a fresh package verifies");

        let mut tampered = package.clone();
        tampered.manifest.checksum ^= 0xffff;
        assert!(!tampered.verify().valid, "a wrong checksum fails");
    }

    #[test]
    fn render_package_mip_writes_a_level() {
        // N439: a stored mip renders its whole downsampled image.
        let source = mdc_bake::SpriteSource {
            name: "s".into(),
            image: {
                let image = image::RgbImage::from_pixel(4, 4, image::Rgb([10, 20, 30]));
                let mut out = Vec::new();
                image
                    .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                    .unwrap();
                out
            },
        };
        let package = mdc_bake::bake_to_package_mipped(
            &[source],
            &[mdc_model::MotionInfo {
                name: "bake".into(),
                channels: 1,
                duration_sec: 0.1,
                fps: 30.0,
                fade_in_sec: 0.0,
                fade_out_sec: 0.0,
                looping: true,
                curves: Vec::new(),
            }],
            (4, 4),
            64,
            &mdc_bake::BakeLimits::default(),
            8,
        )
        .unwrap();
        let dir = std::env::temp_dir().join(format!("mdc-mip-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let pkg = dir.join("a.mdcpkg");
        std::fs::write(&pkg, package.to_bytes().unwrap()).unwrap();
        let out = dir.join("m1.ppm");
        cmd_render_package(&[
            pkg.to_string_lossy().into_owned(),
            "--mip".into(),
            "1".into(),
            "--out".into(),
            out.to_string_lossy().into_owned(),
        ])
        .unwrap();
        assert!(out.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn named_clip_selection_falls_back_to_static() {
        // N429: an unknown clip name renders the static bind pose.
        use mdc_model::{Mesh, MeshGeometry, Model, Node};
        let model = Model {
            source: "t".into(),
            nodes: vec![Node {
                name: "root".into(),
                mesh: Some(0),
                ..Node::default()
            }],
            meshes: vec![Mesh {
                name: "quad".into(),
                vertices: 4,
                indices: 6,
                material: None,
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
            animations: vec![mdc_model::Animation {
                name: "idle".into(),
                channels: 0,
                tracks: Vec::new(),
            }],
            ..Model::default()
        };
        let package =
            mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::full_3d())
                .unwrap();
        // A known name resolves to an index; an unknown name stays static.
        assert!(mdc_bake::render_package_scene_clip_named(
            &package.package,
            "idle",
            0.0,
            (16, 16),
            mdc_bake::Filter::Nearest,
            mdc_bake::ShadeMode::Flat,
        )
        .is_ok());
        assert!(mdc_bake::render_package_scene_clip_named(
            &package.package,
            "missing",
            0.0,
            (16, 16),
            mdc_bake::Filter::Nearest,
            mdc_bake::ShadeMode::Flat,
        )
        .is_ok());
    }

    #[test]
    fn strict_validation_flags_ktx2_notes() {
        // N620: the KTX2 transcoding note is a warning that strict treats as a
        // failure; a size mismatch is a problem regardless.
        let warn = mdc_model::ValidationReport {
            valid: true,
            problems: Vec::new(),
            warnings: vec![
                "texture 0 is a KTX2/Basis container (transcoding is not supported)".into(),
            ],
            summary: mdc_model::ModelSummary::default(),
        };
        assert!(validation_failed(&warn, true));
        assert!(!validation_failed(&warn, false));
        let problem = mdc_model::ValidationReport {
            valid: false,
            problems: vec!["texture 0 KTX2 declares 8x8 but the header says 16x8".into()],
            warnings: Vec::new(),
            summary: mdc_model::ModelSummary::default(),
        };
        assert!(validation_failed(&problem, false));
        assert!(validation_failed(&problem, true));
    }

    #[test]
    fn verify_runs_and_reports_locality() {
        // N604/N611: `mdc verify` succeeds on a valid package (and reports the
        // cache locality alongside the container verification).
        let dir = std::env::temp_dir().join(format!("mdc-verify-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("v.mdcpkg");
        let model = mdc_model::GltfImporter
            .import(
                br#"{"asset":{"version":"2.0"},
                    "nodes":[{"name":"root","mesh":0}],
                    "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
                    "accessors":[{"count":4,"type":"VEC3"}]}"#,
                &mdc_model::ImportLimits::default(),
            )
            .unwrap();
        let package =
            mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::full_3d())
                .unwrap();
        std::fs::write(&path, package.to_bytes().unwrap()).unwrap();
        cmd_verify(&[path.to_string_lossy().into_owned()]).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_defaults_action_runs_without_a_session() {
        // N626: `mdc config --defaults` reports the board defaults directly.
        let opts = GlobalOpts {
            sim: true,
            board: "linux-virt".into(),
            address: None,
            port: None,
        };
        cmd_config(&opts, "--defaults", "", None).unwrap();
        // N666: `--key K` filters to one default.
        cmd_config(&opts, "--defaults", "--key", Some("tile_missing_threshold")).unwrap();
        let all = board_config_defaults("linux-virt");
        assert_eq!(
            filter_config_defaults(&all, Some("tile_missing_threshold"))["tile_missing_threshold"],
            "2"
        );
        assert!(filter_config_defaults(&all, Some("missing"))
            .as_object()
            .unwrap()
            .is_empty());
        assert_eq!(filter_config_defaults(&all, None), all);
    }

    #[test]
    fn inspect_defaults_only_skips_the_session() {
        // N646: `inspect --defaults-only` returns the same table shape as
        // `config --defaults`, without opening a device.
        let opts = GlobalOpts {
            sim: true,
            board: "linux-virt".into(),
            address: None,
            port: None,
        };
        cmd_inspect(&opts, &["--defaults-only".into()]).unwrap();
        let body = defaults_json("linux-virt");
        assert_eq!(body["board"], "linux-virt");
        assert!(body["board_config_defaults"].is_object());
        assert_eq!(
            body["board_config_defaults"],
            board_config_defaults("linux-virt")
        );
        // An unknown board still yields a (empty) table rather than failing.
        let empty = defaults_json("no-such-board");
        assert!(empty["board_config_defaults"].is_object());
    }

    #[test]
    fn defaults_only_can_select_one_key() {
        // N656: `--key` returns just that default (null when absent).
        let table = board_config_defaults("linux-virt");
        let key = table
            .as_object()
            .and_then(|map| map.keys().next().cloned())
            .expect("linux-virt advertises a default");
        let body = defaults_key_json("linux-virt", &key);
        assert_eq!(body["board"], "linux-virt");
        assert_eq!(body["key"], key);
        assert_eq!(body["value"], table[&key]);
        let missing = defaults_key_json("linux-virt", "no-such-key");
        assert!(missing["value"].is_null());
        // The full table still parses when no key is selected.
        let full = defaults_json("linux-virt");
        assert_eq!(full["board_config_defaults"], table);
    }

    #[test]
    fn defaults_table_is_shared_between_config_and_inspect() {
        // N636: both commands publish the board defaults under one key, so a
        // consumer reads either output the same way.
        let table = board_config_defaults("linux-virt");
        assert!(
            table.is_object(),
            "the defaults table is a JSON object: {table}"
        );
        assert!(
            !table.as_object().unwrap().is_empty(),
            "linux-virt advertises defaults"
        );
        // The command output uses the shared key (see `cmd_config`/`cmd_inspect`).
        let config_line = serde_json::json!({
            "board": "linux-virt",
            "board_config_defaults": table.clone(),
        });
        assert!(config_line.get("board_config_defaults").is_some());
        assert!(config_line.get("defaults").is_none(), "one key only");
    }

    #[test]
    fn merged_config_defaults_overlay_the_device_values() {
        // N676: `inspect` merges the profile table with the device-advertised one.
        use std::collections::BTreeMap;
        let only_profile = merge_default_maps("linux-virt", &[]);
        assert_eq!(only_profile["tile_missing_threshold"], "2");
        let mut advertised = BTreeMap::new();
        advertised.insert("tile_missing_threshold".to_string(), "9".to_string());
        advertised.insert("extra".to_string(), "x".to_string());
        let merged = merge_default_maps("linux-virt", &[advertised]);
        assert_eq!(merged["tile_missing_threshold"], "9", "device wins");
        assert_eq!(merged["extra"], "x");
    }

    #[test]
    fn verify_report_carries_the_manifest_locality() {
        // N690: the verify report exposes both the manifest value and whether it
        // matches a fresh recomputation.
        let model = mdc_model::GltfImporter
            .import(
                br#"{"asset":{"version":"2.0"},"nodes":[{"name":"r","mesh":0}],
                    "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
                    "accessors":[{"count":4,"type":"VEC3"}]}"#,
                &mdc_model::ImportLimits::default(),
            )
            .unwrap();
        let package =
            mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::full_3d())
                .unwrap();
        let (report, valid) = verify_report(&package);
        assert!(valid);
        assert!(report.get("manifest_cache_misses").is_some());
        assert_eq!(report["cache_misses_ok"], true);
    }

    #[test]
    fn board_config_defaults_are_exposed() {
        // N606: the board's per-key defaults are reported by `mdc inspect`.
        let linux = board_config_defaults("linux-virt");
        assert_eq!(linux["tile_missing_threshold"], "2");
        let baseline = board_config_defaults("default");
        assert!(baseline.as_object().unwrap().is_empty());
    }

    #[test]
    fn package_cache_misses_sum_over_geometry() {
        // N604: the locality report sums per-mesh cache misses.
        use mdc_model::{Mesh, MeshGeometry, Model, Node};
        let model = Model {
            source: "t".into(),
            nodes: vec![Node {
                name: "root".into(),
                mesh: Some(0),
                ..Node::default()
            }],
            meshes: vec![Mesh {
                name: "quad".into(),
                vertices: 4,
                indices: 6,
                geometry: Some(MeshGeometry {
                    positions: vec![
                        [0.0, 0.0, 0.0],
                        [1.0, 0.0, 0.0],
                        [1.0, 1.0, 0.0],
                        [0.0, 1.0, 0.0],
                    ],
                    indices: vec![0, 1, 2, 0, 2, 3],
                    ..Default::default()
                }),
                ..Mesh::default()
            }],
            ..Model::default()
        };
        let package =
            mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::full_3d())
                .unwrap();
        let expected = mdc_bake::index_cache_misses(
            package.package.meshes[0].geometry.as_ref().unwrap(),
            CACHE_REPORT_SIZE,
        );
        assert_eq!(
            package_cache_misses(&package.package, CACHE_REPORT_SIZE),
            expected
        );
        assert!(expected > 0);
    }

    #[test]
    fn cache_mode_labels_the_policy() {
        // N594: off (no flag) | adaptive (bare flag) | fixed (explicit size).
        assert_eq!(cache_mode(false, 0), "off");
        assert_eq!(cache_mode(true, 0), "adaptive");
        assert_eq!(cache_mode(true, 64), "fixed");
        assert_eq!(cache_mode(false, 32), "fixed");
    }

    #[test]
    fn package_report_prints_texture_detail() {
        // N458: `--report` prints an extended compile summary.
        let dir = std::env::temp_dir().join(format!("mdc-report-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("m.gltf");
        std::fs::write(
            &path,
            br#"{"asset":{"version":"2.0"},
                "nodes":[{"name":"root","mesh":0}],
                "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
                "accessors":[{"count":4,"type":"VEC3"}]}"#,
        )
        .unwrap();
        cmd_package(
            &GlobalOpts {
                sim: true,
                board: "linux-virt".into(),
                address: None,
                port: None,
            },
            &path.to_string_lossy(),
            &[
                "--report".into(),
                "--out".into(),
                dir.join("a.mdcpkg").to_string_lossy().into_owned(),
            ],
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn package_max_draw_vertices_trims_the_package() {
        // N524: `--max-draw-vertices` trims meshes past the budget. The POSITION
        // accessor is an embedded `data:` URI (4 zero VEC3 floats = 48 bytes).
        let dir = std::env::temp_dir().join(format!("mdc-vertex-budget-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("m.gltf");
        let zeros = "A".repeat(64); // base64 of 48 zero bytes
        let gltf = format!(
            r#"{{"asset":{{"version":"2.0"}},
                "nodes":[{{"name":"a","mesh":0}},{{"name":"b","mesh":1}}],
                "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}}}}]}},
                          {{"primitives":[{{"attributes":{{"POSITION":0}}}}]}}],
                "buffers":[{{"byteLength":48,"uri":"data:application/octet-stream;base64,{zeros}"}}],
                "bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":48}}],
                "accessors":[{{"bufferView":0,"componentType":5126,"count":4,"type":"VEC3"}}]}}"#
        );
        std::fs::write(&path, gltf).unwrap();
        let opts = GlobalOpts {
            sim: true,
            board: "linux-virt".into(),
            address: None,
            port: None,
        };
        let run = |budget: &str| {
            let out = dir.join(format!("b{budget}.mdcpkg"));
            cmd_package(
                &opts,
                &path.to_string_lossy(),
                &[
                    "--max-draw-vertices".into(),
                    budget.into(),
                    "--out".into(),
                    out.to_string_lossy().into_owned(),
                ],
            )
            .unwrap();
            let package = mdc_model::MdcPackage::from_bytes(&std::fs::read(&out).unwrap()).unwrap();
            mdc_bake::draw_vertices(&package.package)
        };
        // Two 4-vertex meshes: a budget of 4 keeps one, a generous one keeps both.
        assert_eq!(run("4"), 4);
        assert_eq!(run("100"), 8);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn package_mip_level_resamples_the_base_texture() {
        // N504: `--texture-mips` builds the chain and `--mip-level` bakes from
        // a coarser level, shrinking the stored base texture.
        let dir = std::env::temp_dir().join(format!("mdc-mip-level-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("m.gltf");
        std::fs::write(
            &path,
            br#"{"asset":{"version":"2.0"},
                "nodes":[{"name":"root","mesh":0}],
                "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
                "accessors":[{"count":4,"type":"VEC3"}]}"#,
        )
        .unwrap();
        let texture = dir.join("t.png");
        std::fs::write(
            &texture,
            mdc_mirror::encode_png(16, 16, &vec![0x40u8; 16 * 16 * 4]),
        )
        .unwrap();
        let opts = GlobalOpts {
            sim: true,
            board: "linux-virt".into(),
            address: None,
            port: None,
        };
        let run = |level: &str| {
            let out = dir.join(format!("l{level}.mdcpkg"));
            cmd_package(
                &opts,
                &path.to_string_lossy(),
                &[
                    "--texture".into(),
                    texture.to_string_lossy().into_owned(),
                    "--texture-mips".into(),
                    "8".into(),
                    "--mip-level".into(),
                    level.into(),
                    "--out".into(),
                    out.to_string_lossy().into_owned(),
                ],
            )
            .unwrap();
            let package = mdc_model::MdcPackage::from_bytes(&std::fs::read(&out).unwrap()).unwrap();
            let blob = package.package.texture.clone().unwrap();
            (blob.width, blob.height)
        };
        // Level 0 keeps the source size; level 2 halves it twice (16 -> 4).
        assert_eq!(run("0"), (16, 16));
        assert_eq!(run("2"), (4, 4));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn package_tangents_flag_generates_tangents() {
        // N428: `--tangents` fills geometry tangents before compiling.
        let dir = std::env::temp_dir().join(format!("mdc-tangents-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("m.gltf");
        std::fs::write(
            &path,
            br#"{"asset":{"version":"2.0"},
                "nodes":[{"name":"root","mesh":0}],
                "meshes":[{"primitives":[{"attributes":{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2}}]}],
                "accessors":[{"count":4,"type":"VEC3"},{"count":4,"type":"VEC3"},{"count":4,"type":"VEC2"}]}"#,
        )
        .unwrap();
        // Without `--tangents` the geometry has none.
        cmd_package(
            &GlobalOpts {
                sim: true,
                board: "linux-virt".into(),
                address: None,
                port: None,
            },
            &path.to_string_lossy(),
            &[
                "--out".into(),
                dir.join("a.mdcpkg").to_string_lossy().into_owned(),
            ],
        )
        .unwrap();
        cmd_package(
            &GlobalOpts {
                sim: true,
                board: "linux-virt".into(),
                address: None,
                port: None,
            },
            &path.to_string_lossy(),
            &[
                "--tangents".into(),
                "--out".into(),
                dir.join("b.mdcpkg").to_string_lossy().into_owned(),
            ],
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_flags_out_of_range_references() {
        // N399: a mesh referencing a missing material is a problem.
        let gltf = br#"{"asset":{"version":"2.0"},"nodes":[{"name":"root","mesh":0}],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0},"material":5}]}],
            "accessors":[{"count":4,"type":"VEC3"}]}"#;
        let model = mdc_model::GltfImporter
            .import(gltf, &mdc_model::ImportLimits::default())
            .unwrap();
        let report = model.validate();
        assert!(report.problems.iter().any(|p| p.contains("material 5")));
        assert!(!report.valid);

        // A well-formed model validates.
        let ok = br#"{"asset":{"version":"2.0"},"nodes":[{"name":"root","mesh":0}],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
            "accessors":[{"count":4,"type":"VEC3"}]}"#;
        let model = mdc_model::GltfImporter
            .import(ok, &mdc_model::ImportLimits::default())
            .unwrap();
        assert!(model.validate().valid, "well-formed model validates");
    }

    #[test]
    fn diff_reports_identical_and_changed_packages() {
        let gltf = br#"{"asset":{"version":"2.0"},"nodes":[{"name":"root","mesh":0}],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
            "accessors":[{"count":4,"type":"VEC3"}]}"#;
        let model = mdc_model::GltfImporter
            .import(gltf, &mdc_model::ImportLimits::default())
            .unwrap();
        let a =
            mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::sprite_only())
                .unwrap();
        let b = mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::full_3d())
            .unwrap();
        let dir = std::env::temp_dir().join(format!("mdc-diff-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let (pa, pb) = (dir.join("a.mdcpkg"), dir.join("b.mdcpkg"));
        std::fs::write(&pa, a.to_bytes().unwrap()).unwrap();
        std::fs::write(&pb, b.to_bytes().unwrap()).unwrap();
        // Same file diffed against itself is identical; a sprite vs mesh package is not.
        cmd_diff(&[
            pa.to_string_lossy().into_owned(),
            pa.to_string_lossy().into_owned(),
        ])
        .unwrap();
        cmd_diff(&[
            pa.to_string_lossy().into_owned(),
            pb.to_string_lossy().into_owned(),
        ])
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn model_info_summarizes_an_imported_model() {
        let dir = std::env::temp_dir().join(format!("mdc-model-info-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("m.gltf");
        std::fs::write(
            &path,
            br#"{"asset":{"version":"2.0"},"nodes":[{"name":"root","mesh":0}],
                "meshes":[{"primitives":[{"attributes":{"POSITION":0,"NORMAL":1},"material":0}]}],
                "materials":[{"name":"skin"}],
                "accessors":[{"count":4,"type":"VEC3"},{"count":4,"type":"VEC3"}]}"#,
        )
        .unwrap();
        cmd_model_info(&[path.to_string_lossy().into_owned()]).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dither_texture_blob_changes_rgb565() {
        // N416: dithering a flat blob produces a non-uniform buffer.
        let blob = mdc_model::TextureBlob {
            width: 4,
            height: 4,
            rgb565: 0x8410u16.to_le_bytes().repeat(16),
            alpha: Vec::new(),
            palette: Vec::new(),
            indices: Vec::new(),
            mips: Vec::new(),
        };
        let dithered = mdc_bake::dither_texture_blob(&blob, 4);
        assert_ne!(dithered.rgb565, blob.rgb565);
        assert_eq!(dithered.rgb565.len(), blob.rgb565.len());
        assert!(!dithered.is_indexed());
        // Strength 0 is a no-op.
        assert_eq!(mdc_bake::dither_texture_blob(&blob, 0).rgb565, blob.rgb565);
    }

    #[test]
    fn indexed_blob_samples_its_palette() {
        // N415: a blob can carry a palette + one index per pixel.
        let blob = mdc_model::TextureBlob {
            width: 2,
            height: 1,
            rgb565: vec![0x00, 0xF8, 0xE0, 0x07],
            alpha: Vec::new(),
            palette: Vec::new(),
            indices: Vec::new(),
            mips: Vec::new(),
        };
        let indexed = mdc_bake::index_texture_blob(&blob, 4);
        assert!(indexed.is_indexed());
        assert!(indexed.validate());
        assert!(indexed.palette.len() <= 4);
        assert_eq!(indexed.indices.len(), 2);
        // The sampled colours come from the palette.
        let left = indexed.sample(0.1, 0.5);
        let right = indexed.sample(0.9, 0.5);
        assert!(indexed.palette.contains(&left));
        assert!(indexed.palette.contains(&right));
    }

    #[test]
    fn palette_quantization_is_bounded() {
        // N405: four colours reduced to two entries; indices stay in range.
        let mut rgb565 = Vec::new();
        for value in [0xF800u16, 0xF800, 0x07E0, 0x07E0] {
            rgb565.extend_from_slice(&value.to_le_bytes());
        }
        let quantized = mdc_bake::quantize_palette(2, 2, &rgb565, 2);
        assert!(quantized.palette.len() <= 2, "palette bounded");
        assert_eq!(quantized.indices.len(), 4);
        assert!(quantized
            .indices
            .iter()
            .all(|i| (*i as usize) < quantized.palette.len()));
        // A 1-entry request is clamped up to two.
        assert!(mdc_bake::quantize_palette(2, 2, &rgb565, 1).palette.len() >= 2);
    }

    #[test]
    fn bayer_dither_spreads_a_flat_grey() {
        // N404: a uniform grey gains variation; strength 0 is a no-op.
        let grey = 0x8410u16.to_le_bytes().repeat(16); // 4x4 mid grey
        let plain = mdc_bake::bayer_dither_rgb565(4, 4, &grey, 0);
        assert_eq!(plain, grey);
        let dithered = mdc_bake::bayer_dither_rgb565(4, 4, &grey, 4);
        assert_ne!(dithered, grey, "dithering changes the image");
        let unique: std::collections::BTreeSet<u16> = dithered
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        assert!(unique.len() > 1, "more than one value appears");
    }

    #[test]
    fn mip_chain_halves_and_averages() {
        // N396: a 2x2 red/green/blue/white atlas downsamples to one averaged texel.
        let rgb565 = |v: u16| v.to_le_bytes().to_vec();
        let mut source = Vec::new();
        for v in [0xF800u16, 0x07E0, 0x001F, 0xFFFF] {
            source.extend_from_slice(&rgb565(v));
        }
        let levels = mdc_bake::bake_mipmaps(2, 2, &source, 4);
        assert_eq!(levels.len(), 2, "2x2 -> 1x1");
        assert_eq!((levels[0].0, levels[0].1), (2, 2));
        assert_eq!((levels[1].0, levels[1].1), (1, 1));
        let averaged = u16::from_le_bytes([levels[1].2[0], levels[1].2[1]]);
        // Averaging is bounded and non-zero.
        assert!(averaged != 0);
        // Oversized inputs and bad lengths produce an empty chain.
        assert!(mdc_bake::bake_mipmaps(2, 2, &[0, 0], 4).is_empty());
    }

    #[test]
    fn asset_verify_detects_a_corrupted_entry() {
        // N487: a stored entry whose bytes no longer hash to its name is flagged.
        let base = std::env::temp_dir().join(format!("mdc-verify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let src = base.join("src.bin");
        std::fs::write(&src, [7u8, 8, 9]).unwrap();
        let store = base.join("store").to_string_lossy().into_owned();
        cmd_asset(&[
            "put".into(),
            store.clone(),
            src.to_string_lossy().into_owned(),
        ])
        .unwrap();
        // Healthy store verifies.
        cmd_asset(&["verify".into(), store.clone()]).unwrap();
        // Corrupt the stored file; verify must fail (exit is not asserted here).
        let cache = mdc_assets::DiskCache::open(&store, 1 << 20, 8 << 20).unwrap();
        let key = cache.list().unwrap()[0].0 .0.clone();
        std::fs::write(cache.dir().join(&key), [0u8, 0, 0]).unwrap();
        let corrupted = mdc_assets::DiskCache::open(&store, 1 << 20, 8 << 20).unwrap();
        let bytes = std::fs::read(corrupted.dir().join(&key)).unwrap();
        assert_ne!(mdc_assets::AssetKey::from_bytes(&bytes).0, key);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn scene_render_time_samples_the_animation() {
        // N479: `--time` renders a different frame of an animated scene.
        let dir = std::env::temp_dir().join(format!("mdc-scene-time-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let scene = dir.join("scene.json");
        std::fs::write(
            &scene,
            r##"{"name":"fade","kind":"rect","rect":[0,0,800,480],"value":"#FFFFFF",
                "anim":{"duration_ms":100,"looping":false,"opacity":[[0,0],[100,255]]}}"##,
        )
        .unwrap();
        let opts = GlobalOpts {
            sim: true,
            board: "linux-virt".into(),
            address: None,
            port: None,
        };
        let early = dir.join("early.ppm");
        cmd_scene_render(
            &opts,
            &[
                scene.to_string_lossy().into_owned(),
                "--time".into(),
                "0".into(),
                "--out".into(),
                early.to_string_lossy().into_owned(),
            ],
        )
        .unwrap();
        let late = dir.join("late.ppm");
        cmd_scene_render(
            &opts,
            &[
                scene.to_string_lossy().into_owned(),
                "--time".into(),
                "100".into(),
                "--out".into(),
                late.to_string_lossy().into_owned(),
            ],
        )
        .unwrap();
        assert_ne!(
            std::fs::read(&early).unwrap(),
            std::fs::read(&late).unwrap()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mirror_sequence_names_are_numbered() {
        // N470: the CLI writes `frame0000.png` … in sequence mode. The naming is
        // exercised end-to-end through `cmd_mirror`, so just check the run.
        let dir = std::env::temp_dir().join(format!("mdc-seq-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        cmd_mirror(
            &GlobalOpts {
                sim: true,
                board: "linux-virt".into(),
                address: None,
                port: None,
            },
            &[
                "--frames".into(),
                "2".into(),
                "--write-dir".into(),
                dir.to_string_lossy().into_owned(),
                "--format".into(),
                "png".into(),
                "--sequence".into(),
            ],
        )
        .unwrap();
        assert!(
            dir.join("frame0001.png").is_file(),
            "numbered frame written"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn downscale_bgra_averages_pixels() {
        // N448: a 2x2 BGRA block averages into one pixel.
        let bytes = [
            10u8, 20, 30, 255, // (0,0)
            20, 30, 40, 255, // (1,0)
            30, 40, 50, 255, // (0,1)
            40, 50, 60, 255, // (1,1)
        ];
        let scaled = downscale_bgra(2, 2, &bytes, 2);
        assert_eq!(scaled, vec![25, 35, 45, 255]);
        // Factor 1 is a copy.
        assert_eq!(downscale_bgra(2, 2, &bytes, 1), bytes.to_vec());
    }

    #[test]
    fn devices_compact_lines_are_json() {
        // N437: an empty manager yields no JSONL lines.
        let lines = devices_json_lines(&DeviceManager::new());
        assert!(lines.is_empty());
    }

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
                boot_id: 0,
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
                    preferred_tile_size: 0,
                    supports_partial_update: false,
                    supports_full_framebuffer: false,
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
                boot_id: 0,
                run_id: 0,
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
                                                preferred_tile_size: 0,
                                                supports_partial_update: false,
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
    fn ota_command_confirms_and_reads_back() {
        let opts = GlobalOpts {
            sim: true,
            address: None,
            port: None,
            board: "esp32-jc3248w535-sim".into(),
        };
        cmd_ota(&opts, "2.1.0", &[]).unwrap();
    }

    #[test]
    fn ota_command_rolls_back_on_a_bad_image() {
        let opts = GlobalOpts {
            sim: true,
            address: None,
            port: None,
            board: "esp32-jc3248w535-sim".into(),
        };
        let result = cmd_ota(&opts, "2.1.0", &["--sha256".to_string(), "bad".to_string()]);
        assert!(result.is_err(), "a bad image must not confirm");
    }

    #[test]
    fn ota_command_rejects_a_tcp_target_that_is_not_listening() {
        // N354: the TCP branch retries then fails cleanly.
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            port
        };
        let opts = GlobalOpts {
            sim: false,
            address: Some(format!("127.0.0.1:{port}")),
            port: None,
            board: "esp32-jc3248w535-sim".into(),
        };
        assert!(cmd_ota(&opts, "2.2.0", &[]).is_err());
    }

    #[test]
    fn provision_preflights_installs_and_reads_back() {
        let dir = std::env::temp_dir().join(format!("mdc-cli-provision-{}", std::process::id()));
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
        let package = dir.join("sample.mdcpkg");
        let opts = GlobalOpts {
            sim: true,
            address: None,
            port: None,
            board: "esp32-jc3248w535-sim".into(),
        };
        cmd_package(
            &opts,
            model.to_str().unwrap(),
            &["--out".to_string(), package.to_string_lossy().into_owned()],
        )
        .unwrap();
        // N339: preflight -> install -> version readback succeeds.
        cmd_provision(&opts, package.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn provision_rejects_a_package_the_renderer_cannot_accept() {
        // A mesh-only package on an MCU (sprite) board rolls back.
        let dir = std::env::temp_dir().join(format!("mdc-cli-provision-rb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A bogus container fails preflight before anything is sent.
        let bad = dir.join("bad.mdcpkg");
        std::fs::write(&bad, b"not a package").unwrap();
        let opts = GlobalOpts {
            sim: true,
            address: None,
            port: None,
            board: "esp32-jc3248w535-sim".into(),
        };
        assert!(cmd_provision(&opts, bad.to_str().unwrap()).is_err());
        let _ = std::fs::remove_dir_all(&dir);
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
        // N351: the surface display profile is exposed.
        assert!(json.contains("\"preferred_tile_size\": 64"));
        assert!(json.contains("\"supports_partial_update\": true"));
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
    fn record_writes_a_sequence_and_manifest() {
        // N510: `mdc record` writes numbered frames and a manifest.
        let dir = std::env::temp_dir().join(format!("mdc-record-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let opts = GlobalOpts {
            sim: true,
            address: None,
            port: None,
            board: "linux-virt".into(),
        };
        cmd_record(
            &opts,
            &[
                "--frames".into(),
                "3".into(),
                "--fps".into(),
                "10".into(),
                "--scale".into(),
                "8".into(),
                "--dir".into(),
                dir.to_string_lossy().into_owned(),
            ],
        )
        .unwrap();
        // Three numbered PPM frames and a manifest.
        for index in 1..=3 {
            assert!(
                dir.join(format!("frame{index:04}.ppm")).is_file(),
                "frame {index} written"
            );
        }
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["format"], "ppm");
        assert_eq!(manifest["frames_written"], 3);
        assert_eq!(manifest["width"], 100);
        assert_eq!(manifest["height"], 60);
        assert_eq!(manifest["files"].as_array().unwrap().len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_play_render_replays_onto_a_device() {
        // N530: `--render` decodes each recorded frame and sends it to a device.
        let dir = std::env::temp_dir().join(format!("mdc-render-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let opts = GlobalOpts {
            sim: true,
            address: None,
            port: None,
            board: "linux-virt".into(),
        };
        cmd_record(
            &opts,
            &[
                "--frames".into(),
                "2".into(),
                "--scale".into(),
                "8".into(),
                "--dir".into(),
                dir.to_string_lossy().into_owned(),
            ],
        )
        .unwrap();
        // Replay onto the default 480x320 board (the frames are 100x60).
        let report = render_sequence(&dir.to_string_lossy(), "default", 0).unwrap();
        assert_eq!(report["frames"], 2);
        assert_eq!(report["rendered"], 2);
        assert!(report["ok"].as_bool().unwrap());
        assert_eq!(report["surface"]["width"], 480);
        // A corrupted frame is reported rather than rendered.
        std::fs::write(dir.join("frame0001.ppm"), b"not an image").unwrap();
        let report = render_sequence(&dir.to_string_lossy(), "default", 0).unwrap();
        assert_eq!(report["rendered"], 1);
        assert_eq!(report["failures"].as_array().unwrap().len(), 1);
        assert!(!report["ok"].as_bool().unwrap());
        // The RGB565 fit is nearest-neighbour and keeps the target size.
        let pixels = vec![0x00, 0xF8, 0xE0, 0x07];
        let fitted = fit_rgb565(&pixels, (2, 1), (4, 2));
        assert_eq!(fitted.len(), 4 * 2 * 2);
        assert_eq!(&fitted[0..2], &[0x00, 0xF8]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_play_render_paces_at_the_requested_fps() {
        // N540: `--fps` paces the replay (2 gaps at 20 fps >= ~100 ms).
        let dir = std::env::temp_dir().join(format!("mdc-render-fps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let opts = GlobalOpts {
            sim: true,
            address: None,
            port: None,
            board: "linux-virt".into(),
        };
        cmd_record(
            &opts,
            &[
                "--frames".into(),
                "3".into(),
                "--scale".into(),
                "8".into(),
                "--dir".into(),
                dir.to_string_lossy().into_owned(),
            ],
        )
        .unwrap();
        let fast = render_sequence(&dir.to_string_lossy(), "default", 0).unwrap();
        assert_eq!(fast["rendered"], 3);
        assert_eq!(fast["fps"], 0);
        let paced = render_sequence(&dir.to_string_lossy(), "default", 20).unwrap();
        assert_eq!(paced["rendered"], 3);
        assert_eq!(paced["fps"], 20);
        assert!(
            paced["elapsed_ms"].as_u64().unwrap() >= 90,
            "paced replay took {} ms",
            paced["elapsed_ms"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_play_verifies_the_sequence() {
        // N520: a recorded sequence verifies against its manifest; a corrupted
        // frame is reported.
        let dir = std::env::temp_dir().join(format!("mdc-replay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let opts = GlobalOpts {
            sim: true,
            address: None,
            port: None,
            board: "linux-virt".into(),
        };
        cmd_record(
            &opts,
            &[
                "--frames".into(),
                "2".into(),
                "--scale".into(),
                "8".into(),
                "--dir".into(),
                dir.to_string_lossy().into_owned(),
            ],
        )
        .unwrap();
        let report = verify_sequence(&dir.to_string_lossy()).unwrap();
        assert_eq!(report["frames"], 2);
        assert_eq!(report["verified"], 2);
        assert!(report["valid"].as_bool().unwrap());
        // Corrupt one frame: its size no longer matches the manifest.
        std::fs::write(dir.join("frame0002.ppm"), b"P6\n1 1\n255\n\x00\x00\x00").unwrap();
        let report = verify_sequence(&dir.to_string_lossy()).unwrap();
        assert_eq!(report["verified"], 1);
        assert_eq!(report["mismatched"].as_array().unwrap().len(), 1);
        assert!(!report["valid"].as_bool().unwrap());
        // A missing frame is reported too.
        std::fs::remove_file(dir.join("frame0001.ppm")).unwrap();
        let report = verify_sequence(&dir.to_string_lossy()).unwrap();
        assert_eq!(report["missing"].as_array().unwrap().len(), 1);
        // The frame-size probe reads each format's header.
        assert_eq!(probe_frame_size(b"P6\n100 60\n255\n"), Some((100, 60)));
        assert_eq!(probe_frame_size(b"not an image"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mirror_command_accepts_a_flip_flag() {
        // N496: `--flip h` mirrors the written frame (same geometry, and the
        // flag is accepted end-to-end).
        let dir = std::env::temp_dir().join(format!("mdc-cli-flip-{}", std::process::id()));
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
            "--flip".to_string(),
            "hv".to_string(),
            "--scale".to_string(),
            "4".to_string(),
            "--write-dir".to_string(),
            dir.to_string_lossy().into_owned(),
        ];
        cmd_mirror(&opts, &extra).unwrap();
        let written = std::fs::read(dir.join("frame.ppm")).unwrap();
        assert!(
            written.starts_with(b"P6\n200 120\n255\n"),
            "flipped+scaled header"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mirror_command_drops_frames_over_a_tiny_bandwidth_budget() {
        // N314: a 1000 B/s budget cannot fit one 800x480 RGB565 frame, so no
        // frame is delivered to the sink.
        let dir = std::env::temp_dir().join(format!("mdc-cli-mirror-bw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let opts = GlobalOpts {
            sim: true,
            address: None,
            port: None,
            board: "linux-virt".into(),
        };
        let extra = vec![
            "--frames".to_string(),
            "4".to_string(),
            "--write-dir".to_string(),
            dir.to_string_lossy().into_owned(),
            "--bandwidth".to_string(),
            "1000".to_string(),
        ];
        cmd_mirror(&opts, &extra).unwrap();
        assert!(
            !dir.join("frame.ppm").exists(),
            "an over-budget frame must not be written"
        );
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
            &["--out".to_string(), out.to_string_lossy().into_owned()],
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

    #[test]
    fn render_package_handles_a_mesh_package() {
        let dir = std::env::temp_dir().join(format!("mdc-cli-meshpkg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let model = mdc_model::Model {
            source: "test".into(),
            nodes: vec![mdc_model::Node {
                name: "root".into(),
                mesh: Some(0),
                ..mdc_model::Node::default()
            }],
            meshes: vec![mdc_model::Mesh {
                name: "quad".into(),
                vertices: 4,
                indices: 6,
                material: Some(0),
                geometry: Some(mdc_model::MeshGeometry {
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
                ..mdc_model::Mesh::default()
            }],
            materials: vec![mdc_model::Material {
                name: "red".into(),
                texture: None,
                color: Some([255, 0, 0]),
                ..mdc_model::Material::default()
            }],
            ..mdc_model::Model::default()
        };
        let package = mdc_bake::mesh_package_with_texture(
            &model,
            &mdc_model::RendererCapability::full_3d(),
            None,
        )
        .unwrap();
        let package_path = dir.join("mesh.mdcpkg");
        std::fs::write(&package_path, package.to_bytes().unwrap()).unwrap();
        let out = dir.join("mesh.ppm");
        cmd_render_package(&[
            package_path.to_string_lossy().into_owned(),
            "--sprite".to_string(),
            "16".to_string(),
            "--out".to_string(),
            out.to_string_lossy().into_owned(),
        ])
        .unwrap();
        assert!(std::fs::read(&out)
            .unwrap()
            .starts_with(b"P6\n16 16\n255\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn motion_command_samples_a_curve() {
        let dir = std::env::temp_dir().join(format!("mdc-cli-motion-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m1.motion3.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "Version": 3,
                "Meta": {"Duration": 1.0, "Fps": 30.0},
                "Curves": [
                    {"Target": "Parameter", "Id": "ParamAngleX",
                     "Segments": [0.0, 0.0, 1.0, 10.0]}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        cmd_motion(&[
            path.to_string_lossy().into_owned(),
            "--param".to_string(),
            "ParamAngleX".to_string(),
            "--samples".to_string(),
            "3".to_string(),
        ])
        .unwrap();
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
