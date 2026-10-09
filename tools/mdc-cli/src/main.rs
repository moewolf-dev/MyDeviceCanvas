//! MyDeviceCanvas CLI — sim-first host tooling (Apache-2.0).
use image::imageops::FilterType;
use mdc_core::{ConnectionState, DeviceManager, Frame, Session};
use mdc_protocol::VERSION;
use mdc_simulator::{BoardProfile, FakeDevice, SimMetrics};
use mdc_transport::{MemoryLink, TcpTransport};
use serde::Serialize;
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
            "--board" => {
                i += 1;
                if i < args.len() {
                    opts.board = args[i].clone();
                }
            }
            other if other.starts_with("--address=") => {
                opts.address = Some(other["--address=".len()..].into());
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

fn cmd_send_image(
    opts: &GlobalOpts,
    path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
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

fn usage() {
    println!(
        "mdc [--sim] [--board id] [--address host:port] \
         devices|inspect|send-image <path>|benchmark [N]|simulate"
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
