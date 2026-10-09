//! MyDeviceCanvas CLI — sim-first host tooling (Apache-2.0).
use image::imageops::FilterType;
use mdc_core::{ConnectionState, DeviceManager, Frame, Session};
use mdc_discovery::{
    CombinedDiscovery, DiscoveryProvider, Endpoint, MockMdnsProvider, PairingStore,
};
use mdc_flasher::{flags_for_board, EspToolFlasher, SimFlasher};
use mdc_protocol::{InputEvent, OtaCommand, VERSION};
use mdc_provision::{Artifact, InstallPlan, Installer, PortLeases};
use mdc_simulator::{BoardProfile, FakeDevice, SimMetrics};
use mdc_transport::{MemoryLink, TcpTransport};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::{Arc, Mutex};
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
        })
        .collect();
    serde_json::to_string_pretty(&list).unwrap_or_else(|_| "[]".into())
}

fn inspect_json(manager: &DeviceManager, metrics: Option<&SimMetrics>) -> String {
    #[derive(Serialize)]
    struct Inspect {
        protocol: String,
        devices: Vec<DeviceDto>,
        metrics: Option<SimMetrics>,
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
        })
        .collect();
    serde_json::to_string_pretty(&Inspect {
        protocol: format!("{}.{}", VERSION.major, VERSION.minor),
        devices,
        metrics: metrics.cloned(),
    })
    .unwrap_or_else(|_| "{}".into())
}

/// Handshake FakeDevice over MemoryLink; returns host session, peer, and shared manager.
fn open_sim(
    board: &str,
) -> Result<(Session<MemoryLink>, FakeDevice, Arc<Mutex<DeviceManager>>), Box<dyn std::error::Error>>
{
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

fn with_sim_or_empty<F>(
    opts: &GlobalOpts,
    f: F,
) -> Result<(), Box<dyn std::error::Error>>
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

fn cmd_inspect(opts: &GlobalOpts) -> Result<(), Box<dyn std::error::Error>> {
    with_sim_or_empty(opts, |mgr, metrics| {
        println!("{}", inspect_json(mgr, metrics));
        Ok(())
    })
}

#[cfg(feature = "serial")]
fn open_serial_session(
    port: &str,
) -> Result<Session<mdc_transport::SerialTransport>, Box<dyn std::error::Error>> {
    use mdc_transport::{is_candidate_serial_port, SerialConfig, SerialOpenGuard, SerialTransport};
    if !is_candidate_serial_port(port) {
        return Err(format!("port {port} does not look like an ESP32 USB-serial candidate").into());
    }
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
            let permanent = e.to_string().contains("Permission") || e.to_string().contains("Access");
            let backoff = guard.note_failure(permanent);
            return Err(format!(
                "serial open failed ({e}); retry after {backoff}ms (gen={gen})"
            )
            .into());
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

fn cmd_send_image(
    opts: &GlobalOpts,
    path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
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
        println!("sent frame to {addr}");
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

fn cmd_benchmark(
    opts: &GlobalOpts,
    frames: u32,
) -> Result<(), Box<dyn std::error::Error>> {
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
    println!(
        "benchmark frames_sent={sent} frames_acked≈{acked} device_frames={} acks={}",
        metrics.frames_received, metrics.acks_sent
    );
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

fn cmd_flash_sim(
    opts: &GlobalOpts,
    image: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = Path::new(image);
    let bytes = std::fs::read(path)?;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let board = opts.board.clone();
    let _flags = flags_for_board(&board).ok_or("unknown board for flash flags")?;
    let plan = InstallPlan {
        port: opts
            .port
            .clone()
            .unwrap_or_else(|| "sim:///flash".into()),
        artifact: Artifact {
            board_id: board.clone(),
            mcu: "esp32-s3".into(),
            runtime: "0.1.0".into(),
            protocol_major: VERSION.major,
            protocol_minor: VERSION.minor,
            image_size: bytes.len() as u64,
            sha256: hash,
            source: "cli-flash-sim".into(),
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
    std::env::temp_dir().join("mdc-pairing-store.json")
}

fn cmd_pair(device_id: &str, token: &str) -> Result<(), Box<dyn std::error::Error>> {
    let path = pairing_path();
    let mut store = PairingStore::load_json(&path).unwrap_or_default();
    store.pair(device_id, token)?;
    store.save_json(&path)?;
    println!("paired device_id={device_id} store={}", path.display());
    Ok(())
}

fn cmd_unpair(device_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let path = pairing_path();
    let mut store = PairingStore::load_json(&path).unwrap_or_default();
    if store.revoke(device_id) {
        store.save_json(&path)?;
        println!("revoked pairing for {device_id}");
    } else {
        println!("no pairing for {device_id}");
    }
    Ok(())
}

fn cmd_authorize(device_id: &str, kind: &str, value: &str) -> Result<(), Box<dyn std::error::Error>> {
    let path = pairing_path();
    let store = PairingStore::load_json(&path).unwrap_or_default();
    let endpoint = match kind {
        "ws" | "websocket" => Endpoint::WebSocket {
            address: value.into(),
        },
        "serial" | "port" => Endpoint::Serial {
            port: value.into(),
        },
        "memory" => Endpoint::Memory {
            label: value.into(),
        },
        other => return Err(format!("unknown endpoint kind {other}").into()),
    };
    match store.authorize(device_id, &endpoint) {
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
    assert!(tile_err.is_err(), "tile must be blocked until full frame ACK");
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

fn usage() {
    println!(
        "mdc [--sim] [--board id] [--address host:port] [--port /dev/cu.usbmodem*] \
         devices|inspect|send-image <path>|benchmark [N]|simulate|discover|\
flash-sim <merged.bin>|h06-demo|pair <id> <token>|unpair <id>|\
authorize <id> <ws|serial|memory> <ep>|switch-demo"
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
            cmd_inspect(&o)
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
                eprintln!("authorize <device_id> <ws|serial|memory> <endpoint>");
                std::process::exit(2);
            }
            cmd_authorize(&id, &kind, &value)
        }
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
}
