//! Print JSON metrics from last `mdc simulate` or a fresh FakeDevice handshake.
//!
//! N317: `--jsonl N` streams a bounded JSONL time series (one compact sample per
//! line). Samples are written as they are produced and never accumulated, so a
//! long watch does not grow memory.
use mdc_core::{ConnectionState, Frame, Session};
use mdc_simulator::{BoardProfile, FakeDevice};
use std::io::Write;
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

/// N317: one JSONL sample per tick. Bounded memory: each line is emitted before
/// the next sample is produced.
fn watch<W: Write>(
    count: u32,
    interval_ms: u64,
    board: &str,
    mut out: W,
) -> Result<(), Box<dyn std::error::Error>> {
    let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id(board));
    let mut session = Session::new(host);
    session.connect()?;
    device.poll()?;
    session.poll()?;
    if session.state != ConnectionState::Ready {
        return Err("not ready".into());
    }
    let interval = interval_ms.max(1);
    for sample in 0..count {
        device.clock.advance(interval);
        // Emit a diagnostic on every tick; that is the channel `mdc inspect`
        // reports on, and it keeps this series independent of the frame path.
        let _ = device.emit_metric("tick", f64::from(sample), None);
        session.poll()?;
        let diagnostics = session.take_diagnostics().len();
        let line = serde_json::json!({
            "sample": sample,
            "tick": device.clock.tick,
            "firmware": device.firmware(),
            "diagnostics": diagnostics,
            "diagnostics_total": device.metrics_snapshot().diagnostics_emitted,
        });
        writeln!(out, "{}", serde_json::to_string(&line)?)?;
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag_value = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let board = flag_value("--board").unwrap_or_else(|| "default".into());
    if let Some(count) = flag_value("--jsonl").and_then(|v| v.parse::<u32>().ok()) {
        let interval = flag_value("--interval")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        if let Err(error) = watch(count, interval, &board, std::io::stdout()) {
            eprintln!("inspector error: {error}");
            std::process::exit(1);
        }
        return;
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_lines(bytes: &[u8]) -> Vec<serde_json::Value> {
        String::from_utf8(bytes.to_vec())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn jsonl_series_is_one_bounded_sample_per_tick() {
        let mut out = Vec::new();
        watch(4, 100, "linux-virt", &mut out).unwrap();
        let lines = parse_lines(&out);
        assert_eq!(lines.len(), 4);
        // Samples are ordered and their tick advances by the interval.
        assert_eq!(lines[0]["sample"], 0);
        assert_eq!(lines[3]["sample"], 3);
        assert_eq!(lines[1]["tick"].as_u64().unwrap(), 200);
        assert_eq!(lines[3]["tick"].as_u64().unwrap(), 400);
        assert_eq!(lines[3]["diagnostics"], 1);
        assert_eq!(lines[3]["diagnostics_total"], 4);
    }

    #[test]
    fn zero_count_produces_no_lines() {
        let mut out = Vec::new();
        watch(0, 10, "default", &mut out).unwrap();
        assert!(out.is_empty());
    }
}
