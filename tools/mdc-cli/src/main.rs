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
        gate_network_display(
            &device_id,
            &Endpoint::Tcp {
                address: addr.clone(),
            },
        )?;
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
    println!("paired device_id={device_id} store={}", path.display());
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

fn usage() {
    println!(
        "mdc [--sim] [--board id] [--address host:port] [--port /dev/cu.usbmodem*] \
         devices|inspect|send-image <path>|benchmark [N]|simulate|discover|\
flash-sim <merged.bin>|h06-demo|sim-accept|ota-confirm|pair <id> <token>|unpair <id>|\
authorize <id> <ws|tcp|serial|memory> <ep>|switch-demo"
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
        Some("sim-accept") => cmd_sim_accept(&opts),
        Some("ota-confirm") => cmd_ota_confirm(&opts),
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
}
