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

/// N370: the host process resident set size in bytes, when the platform exposes
/// it (Linux: current RSS from `/proc`; other unix: peak RSS via `getrusage`).
fn resident_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        Some(pages * 4096)
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // macOS/BSD `ru_maxrss` is a peak, measured in bytes.
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

/// N370: `ru_maxrss` is a high-water mark on non-Linux unix (bytes); on Linux
/// `resident_bytes` returns the current value.
fn rss_is_peak() -> bool {
    cfg!(all(unix, not(target_os = "linux")))
}

/// N370: a one-sample host memory snapshot.
fn rss_json() -> serde_json::Value {
    serde_json::json!({
        "rss_bytes": resident_bytes(),
        "peak": rss_is_peak(),
    })
}

/// N410: sample the host RSS `count` times and report the growth. Bounded to
/// 1..=600 samples so a long watch cannot grow the buffer.
fn rss_delta(count: u32, interval_ms: u64) -> serde_json::Value {
    let count = count.clamp(1, 600);
    let interval = interval_ms.max(1);
    let mut samples = Vec::with_capacity(count as usize);
    for index in 0..count {
        samples.push(resident_bytes().unwrap_or(0));
        if index + 1 < count {
            std::thread::sleep(std::time::Duration::from_millis(interval));
        }
    }
    let first = samples.first().copied().unwrap_or(0);
    let last = samples.last().copied().unwrap_or(0);
    let min = samples.iter().copied().min().unwrap_or(0);
    let max = samples.iter().copied().max().unwrap_or(0);
    serde_json::json!({
        "samples": count,
        "first": first,
        "last": last,
        "min": min,
        "max": max,
        "delta": last as i64 - first as i64,
        "peak": rss_is_peak(),
    })
}

/// N509: sample host memory while cycling a bounded buffer, and report the peak
/// growth over the baseline. The buffer is released each iteration, so the peak
/// is the high-water mark of one allocation, not an accumulation.
fn heap_peak(count: u32, interval_ms: u64, size_kb: u64) -> serde_json::Value {
    let count = count.clamp(1, 600);
    let interval = interval_ms.max(1);
    let size = (size_kb.clamp(1, 65_536) * 1024) as usize;
    let baseline = resident_bytes().unwrap_or(0);
    let mut peak = baseline;
    let mut last = baseline;
    for index in 0..count {
        // Touch one byte per page so the buffer becomes resident.
        let mut buffer = vec![0u8; size];
        for byte in buffer.iter_mut().step_by(4096) {
            *byte = 1;
        }
        last = resident_bytes().unwrap_or(0);
        peak = peak.max(last);
        drop(buffer);
        if index + 1 < count {
            std::thread::sleep(std::time::Duration::from_millis(interval));
        }
    }
    serde_json::json!({
        "samples": count,
        "buffer_bytes": size,
        "baseline": baseline,
        "last": last,
        "peak": peak,
        "growth": peak as i64 - baseline as i64,
        "peak_is_high_water": rss_is_peak(),
    })
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
#[allow(clippy::too_many_arguments)]
fn watch<W: Write>(
    count: u32,
    interval_ms: u64,
    board: &str,
    // N519: when non-zero, each sample also cycles a buffer of this size (KiB)
    // and reports the running peak, so the timeline shows allocation peaks.
    heap_kb: u64,
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
    let heap_size = (heap_kb.clamp(0, 65_536) * 1024) as usize;
    let baseline = resident_bytes().unwrap_or(0);
    let mut peak = baseline;
    for sample in 0..count {
        // N519: touch a buffer, sample, then release it (never accumulates).
        let heap = if heap_size > 0 {
            let mut buffer = vec![0u8; heap_size];
            for byte in buffer.iter_mut().step_by(4096) {
                *byte = 1;
            }
            let rss = resident_bytes().unwrap_or(0);
            peak = peak.max(rss);
            drop(buffer);
            Some(serde_json::json!({
                "buffer_bytes": heap_size,
                "rss_bytes": rss,
                "peak_bytes": peak,
                "growth": peak as i64 - baseline as i64,
            }))
        } else {
            None
        };
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
            "rss_bytes": resident_bytes(),
            // N519: the heap probe for this tick (null unless `--heap`).
            "heap": heap,
            // N460: host-side session counters (parity with `mdc inspect --watch`).
            "session": {
                "packets_sent": session.stats().packets_sent,
                "bytes_sent": session.stats().bytes_sent,
                "bytes_received": session.stats().bytes_received,
                "acks_received": session.stats().acks_received,
                "frames_sent": session.stats().frames_sent,
                "mirror_frames": session.stats().mirror_frames,
            },
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
    // N370: a single bounded host-memory sample (no lingering state).
    if args.iter().any(|a| a == "--rss") {
        println!("{}", rss_json());
        return;
    }
    // N410: a bounded RSS growth sample.
    if args.iter().any(|a| a == "--rss-delta") {
        let count = flag_value("--count")
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(5);
        let interval = flag_value("--interval")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(20);
        println!("{}", rss_delta(count, interval));
        return;
    }
    // N509: a bounded peak-memory probe over a cyclic allocation.
    if args.iter().any(|a| a == "--heap") {
        let count = flag_value("--count")
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(8);
        let interval = flag_value("--interval")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(5);
        let size_kb = flag_value("--size-kb")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1024);
        println!("{}", heap_peak(count, interval, size_kb));
        return;
    }
    if let Some(count) = flag_value("--jsonl").and_then(|v| v.parse::<u32>().ok()) {
        let interval = flag_value("--interval")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let heap_kb = if args.iter().any(|a| a == "--heap") {
            flag_value("--size-kb")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(1024)
        } else {
            0
        };
        if let Err(error) = watch(count, interval, &board, heap_kb, std::io::stdout()) {
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

    #[test]
    fn heap_peak_reports_a_bounded_growth() {
        // N509: the probe samples, releases, and never accumulates.
        let report = heap_peak(3, 1, 512);
        assert_eq!(report["samples"], 3);
        assert_eq!(report["buffer_bytes"], 512 * 1024);
        let peak = report["peak"].as_u64().unwrap();
        let baseline = report["baseline"].as_u64().unwrap();
        assert!(peak >= baseline, "peak is at least the baseline");
        // The growth is an integer delta of the two samples.
        assert_eq!(
            report["growth"].as_i64().unwrap(),
            peak as i64 - baseline as i64
        );
    }

    #[test]
    fn heap_timeline_reports_a_peak_per_tick() {
        // N519: `--jsonl N --heap` adds a heap object to every sample.
        let mut out = Vec::new();
        watch(3, 1, "linux-virt", 256, &mut out).unwrap();
        let lines = parse_lines(&out);
        assert_eq!(lines.len(), 3);
        let mut last_peak = 0u64;
        for line in &lines {
            let heap = &line["heap"];
            assert_eq!(heap["buffer_bytes"], 256 * 1024);
            assert!(heap["rss_bytes"].as_u64().unwrap() > 0);
            let peak = heap["peak_bytes"].as_u64().unwrap();
            assert!(peak >= last_peak, "the peak never decreases");
            last_peak = peak;
        }
        // Without `--heap` the field is null.
        let mut out = Vec::new();
        watch(1, 1, "default", 0, &mut out).unwrap();
        assert!(parse_lines(&out)[0]["heap"].is_null());
    }

    #[test]
    fn heap_peak_clamps_the_probe_bounds() {
        let report = heap_peak(0, 0, 0);
        assert_eq!(report["samples"], 1);
        assert_eq!(report["buffer_bytes"], 1024);
        let huge = heap_peak(1, 1, u64::MAX);
        assert_eq!(huge["buffer_bytes"], 65_536 * 1024);
    }

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
        watch(4, 100, "linux-virt", 0, &mut out).unwrap();
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
        watch(0, 10, "default", 0, &mut out).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn rss_delta_is_bounded_and_well_formed() {
        // N410: a two-sample growth check reports a delta and is capped.
        let report = rss_delta(2, 1);
        assert_eq!(report["samples"], serde_json::json!(2));
        assert!(report.get("delta").is_some());
        assert_eq!(rss_delta(10_000, 1)["samples"], serde_json::json!(600));
    }

    #[test]
    fn jsonl_samples_carry_a_bounded_rss_reading() {
        let mut out = Vec::new();
        watch(2, 10, "default", 0, &mut out).unwrap();
        for line in parse_lines(&out) {
            assert!(line.get("rss_bytes").is_some(), "rss present");
            // N460: session counters ride along.
            assert!(line.get("session").is_some(), "session stats present");
        }
    }

    #[test]
    fn rss_snapshot_is_well_formed() {
        let snapshot = rss_json();
        assert!(snapshot.get("rss_bytes").is_some());
        assert_eq!(snapshot["peak"], serde_json::json!(rss_is_peak()));
        // On the unix hosts this gate runs on, a reading is available and sane.
        #[cfg(unix)]
        {
            let bytes = resident_bytes().expect("rss available");
            assert!(bytes > 0, "rss is positive");
        }
    }
}
