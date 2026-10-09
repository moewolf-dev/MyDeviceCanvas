//! Print JSON metrics from last `mdc simulate` or a fresh FakeDevice handshake.
use mdc_core::{ConnectionState, Frame, Session};
use mdc_simulator::{BoardProfile, FakeDevice};
use std::path::PathBuf;

fn metrics_path() -> PathBuf {
    std::env::temp_dir().join("mdc-last-sim-metrics.json")
}

fn fresh_metrics() -> Result<String, Box<dyn std::error::Error>> {
    let (host, mut device) = FakeDevice::pair(BoardProfile::default());
    let mut session = Session::new(host);
    session.connect()?;
    device.poll()?;
    session.poll()?;
    if session.state != ConnectionState::Ready {
        return Err("not ready".into());
    }
    let w = device.profile().width;
    let h = device.profile().height;
    session.send_frame(Frame {
        surface_id: "main".into(),
        width: w,
        height: h,
        bytes: vec![0u8; usize::from(w) * usize::from(h) * 2],
    })?;
    device.poll()?;
    session.poll()?;
    Ok(serde_json::to_string_pretty(&device.metrics_snapshot())?)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let force_fresh = args.iter().any(|a| a == "--fresh" || a == "simulate");
    let path = metrics_path();
    let json = if !force_fresh && path.is_file() {
        match std::fs::read_to_string(&path) {
            Ok(s) if !s.trim().is_empty() => s,
            _ => match fresh_metrics() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("inspector error: {e}");
                    std::process::exit(1);
                }
            },
        }
    } else {
        match fresh_metrics() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("inspector error: {e}");
                std::process::exit(1);
            }
        }
    };
    println!("{json}");
}
