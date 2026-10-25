//! SIM-R09: long-running host soak with RSS, thread, FD and container sampling.
//!
//! The 60-minute gate is opt-in:
//! ```sh
//! MDC_LONG_SOAK=1 MDC_SOAK_WARMUP_SECS=300 MDC_SOAK_SECS=3600 \
//!   MDC_SOAK_IDLE_SECS=60 cargo test -p mdc-memory-tests --test long_soak -- --nocapture
//! ```
//! Without `MDC_LONG_SOAK` this test returns immediately so normal suites stay
//! fast. The RSS budget defaults to 10 MiB above the post-warmup baseline.
use mdc_core::{
    ConnectionState, ContainerCounts, DeviceManager, Frame, Session, Tile, MAX_DIAGNOSTICS,
    MAX_DISPLAY_TIMINGS, MAX_FINISHED_ACKS, MAX_MANAGER_EVENTS,
};
use mdc_simulator::{BoardProfile, FakeDevice};
use mdc_transport::Transport;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
struct Sample {
    rss_kib: Option<u64>,
    threads: Option<usize>,
    fds: Option<usize>,
    counts: ContainerCounts,
    copy_outstanding: usize,
    transactions: u64,
}

fn tiny_package_bytes() -> Vec<u8> {
    let gltf = br#"{"asset":{"version":"2.0"},"nodes":[{"name":"root","mesh":0}],
        "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
        "accessors":[{"count":4,"type":"VEC3"}]}"#;
    let model = mdc_model::GltfImporter
        .import(gltf, &mdc_model::ImportLimits::default())
        .unwrap();
    mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::sprite_only())
        .unwrap()
        .to_bytes()
        .unwrap()
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn rss_kib() -> Option<u64> {
    let pid = std::process::id().to_string();
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

fn thread_count() -> Option<usize> {
    let pid = std::process::id().to_string();
    #[cfg(target_os = "linux")]
    {
        let output = std::process::Command::new("ps")
            .args(["-o", "nlwp=", "-p", &pid])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        return String::from_utf8_lossy(&output.stdout).trim().parse().ok();
    }
    #[cfg(not(target_os = "linux"))]
    {
        let output = std::process::Command::new("ps")
            .args(["-M", "-p", &pid])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        text.lines().count().checked_sub(1)
    }
}

fn fd_count() -> Option<usize> {
    let path = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    std::fs::read_dir(path).ok().map(|entries| entries.count())
}

fn sample(session: &Session<Box<dyn Transport>>, transactions: u64) -> Sample {
    Sample {
        rss_kib: rss_kib(),
        threads: thread_count(),
        fds: fd_count(),
        counts: session.container_counts(),
        copy_outstanding: session.copy_outstanding(),
        transactions,
    }
}

fn assert_containers_bounded(counts: &ContainerCounts) {
    assert_eq!(counts.rx_bytes, 0);
    assert_eq!(counts.pending_requests, 0);
    assert_eq!(counts.pending_control, 0);
    assert_eq!(counts.pending_frames, 0);
    assert_eq!(counts.held_frames, 0);
    assert_eq!(counts.frame_transactions, 0);
    assert!(counts.finished_acks <= MAX_FINISHED_ACKS);
    assert!(counts.displayed_timings <= MAX_DISPLAY_TIMINGS);
    assert!(counts.diagnostics <= MAX_DIAGNOSTICS);
    assert!(counts.manager_events <= MAX_MANAGER_EVENTS);
    assert!(counts.active_pointers <= mdc_core::MAX_ACTIVE_POINTERS);
}

struct Harness {
    session: Session<Box<dyn Transport>>,
    device: FakeDevice,
    profile: BoardProfile,
    transactions: u64,
    package_installs: u64,
    reconnects: u64,
}

impl Harness {
    fn new(profile: BoardProfile) -> Self {
        let (host, mut device) = FakeDevice::pair(profile.clone());
        let manager = Arc::new(Mutex::new(DeviceManager::new()));
        let mut session = Session::new(Box::new(host) as Box<dyn Transport>);
        session.set_manager(manager);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
        Self {
            session,
            device,
            profile,
            transactions: 0,
            package_installs: 0,
            reconnects: 0,
        }
    }

    fn run_for(
        &mut self,
        duration: Duration,
        mut tick: impl FnMut(&Session<Box<dyn Transport>>, u64),
    ) {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            self.step();
            tick(&self.session, self.transactions);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn step(&mut self) {
        let value = (self.transactions & 0xff) as u8;
        static PACKAGE: Mutex<Option<Vec<u8>>> = Mutex::new(None);
        self.session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: self.profile.width,
                height: self.profile.height,
                bytes: vec![
                    value;
                    usize::from(self.profile.width) * usize::from(self.profile.height) * 2
                ],
            })
            .unwrap();
        self.device.poll().unwrap();
        self.session.poll().unwrap();
        if self.transactions.is_multiple_of(50) && self.device.current_frame_id() > 0 {
            self.session
                .send_tile(Tile {
                    surface_id: "main".into(),
                    base_frame_id: self.device.current_frame_id(),
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                    bytes: vec![0x11, 0x22],
                })
                .unwrap();
            self.device.poll().unwrap();
            self.session.poll().unwrap();
        }
        self.transactions = self.transactions.saturating_add(1);
        if self.transactions.is_multiple_of(3000) {
            let mut package = PACKAGE.lock().unwrap();
            let bytes = package.get_or_insert_with(tiny_package_bytes);
            self.session.install_package(bytes).unwrap();
            self.device.poll().unwrap();
            self.session.poll().unwrap();
            assert!(self.device.package_ready());
            self.package_installs = self.package_installs.saturating_add(1);
        }
        if self.transactions.is_multiple_of(2000) {
            // Exercise endpoint replacement without rebuilding the Session.
            let (host, mut device) = FakeDevice::pair(self.profile.clone());
            self.session.switch_transport(Box::new(host)).unwrap();
            device.poll().unwrap();
            assert_eq!(self.session.poll().unwrap(), ConnectionState::Ready);
            self.device = device;
            self.reconnects = self.reconnects.saturating_add(1);
        }
    }
}

fn counts_json(counts: &ContainerCounts) -> serde_json::Value {
    serde_json::json!({
        "rx_bytes": counts.rx_bytes,
        "pending_requests": counts.pending_requests,
        "pending_control": counts.pending_control,
        "pending_frames": counts.pending_frames,
        "request_deadlines": counts.request_deadlines,
        "finished_acks": counts.finished_acks,
        "displayed_timings": counts.displayed_timings,
        "held_frames": counts.held_frames,
        "diagnostics": counts.diagnostics,
        "config_cache": counts.config_cache,
        "pending_configs": counts.pending_configs,
        "active_pointers": counts.active_pointers,
        "mirror_inbox_frames": counts.mirror_inbox_frames,
        "mirror_base_frames": counts.mirror_base_frames,
        "frame_transactions": counts.frame_transactions,
        "manager_events": counts.manager_events,
        "devices": counts.devices,
    })
}

fn sample_json(sample: &Sample) -> serde_json::Value {
    serde_json::json!({
        "rss_kib": sample.rss_kib,
        "threads": sample.threads,
        "fds": sample.fds,
        "transactions": sample.transactions,
        "copy_outstanding": sample.copy_outstanding,
        "containers": counts_json(&sample.counts),
    })
}

#[test]
fn long_soak_keeps_resources_flat() {
    if std::env::var("MDC_LONG_SOAK").ok().as_deref() != Some("1") {
        eprintln!("MDC_LONG_SOAK is not 1; skipping the RSS/thread/FD soak");
        return;
    }
    let warmup = Duration::from_secs(env_u64("MDC_SOAK_WARMUP_SECS", 5));
    let steady = Duration::from_secs(env_u64("MDC_SOAK_SECS", 10));
    let idle = Duration::from_secs(env_u64("MDC_SOAK_IDLE_SECS", 3));
    let budget_kib = env_u64("MDC_RSS_BUDGET_KIB", 10 * 1024);
    let profile = BoardProfile {
        width: 2,
        height: 2,
        renderer_sprite: true,
        ..BoardProfile::default()
    };
    let mut harness = Harness::new(profile);
    let run_started = Instant::now();

    harness.run_for(warmup, |_, _| {});
    let baseline = sample(&harness.session, harness.transactions);
    eprintln!("R09 baseline {baseline:?}");

    let started = Instant::now();
    let mut early = None;
    let mut midpoint = None;
    harness.run_for(steady, |session, transactions| {
        let elapsed = started.elapsed();
        if early.is_none() && elapsed >= steady / 6 {
            assert_containers_bounded(&session.container_counts());
            let point = sample(session, transactions);
            eprintln!("R09 early {point:?}");
            early = Some(point);
        }
        if midpoint.is_none() && elapsed >= steady / 2 {
            assert_containers_bounded(&session.container_counts());
            let point = sample(session, transactions);
            eprintln!("R09 midpoint {point:?}");
            midpoint = Some(point);
        }
    });
    let end = sample(&harness.session, harness.transactions);
    eprintln!("R09 end {end:?}");
    let rss_growth = match (baseline.rss_kib, end.rss_kib) {
        (Some(before), Some(after)) => Some(after.saturating_sub(before)),
        _ => None,
    };
    assert_containers_bounded(&end.counts);
    if let (Some(baseline_rss), Some(end_rss)) = (baseline.rss_kib, end.rss_kib) {
        let growth = end_rss.saturating_sub(baseline_rss);
        eprintln!("R09 RSS growth {growth} KiB (budget {budget_kib} KiB)");
        assert!(
            growth <= budget_kib,
            "RSS grew by {growth} KiB, budget {budget_kib} KiB"
        );
    }
    if let (Some(before), Some(after)) = (baseline.threads, end.threads) {
        assert!(after <= before + 2, "threads {before} -> {after}");
    }
    if let (Some(before), Some(after)) = (baseline.fds, end.fds) {
        assert!(after <= before + 2, "fds {before} -> {after}");
    }
    assert!(end.transactions > 100, "the soak actually ran");

    std::thread::sleep(idle);
    let idle_sample = sample(&harness.session, harness.transactions);
    eprintln!("R09 idle {idle_sample:?}");
    assert_containers_bounded(&idle_sample.counts);
    assert_eq!(harness.session.copy_outstanding(), 0);

    if let Some(path) = std::env::var_os("MDC_SOAK_REPORT") {
        let commit = std::env::var("MDC_SOAK_COMMIT").ok().or_else(|| {
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        });
        let report = serde_json::json!({
            "mode": "long",
            "source": "tests/memory/tests/long_soak.rs",
            "commit": commit,
            "ok": true,
            "duration_seconds": run_started.elapsed().as_secs(),
            "config": {
                "warmup_seconds": warmup.as_secs(),
                "steady_seconds": steady.as_secs(),
                "idle_seconds": idle.as_secs(),
                "rss_budget_kib": budget_kib,
            },
            "samples": {
                "baseline": sample_json(&baseline),
                "early": early.map(|point| sample_json(&point)),
                "midpoint": midpoint.map(|point| sample_json(&point)),
                "end": sample_json(&end),
                "idle": sample_json(&idle_sample),
            },
            "rss_growth_kib": rss_growth,
            "package_installs": harness.package_installs,
            "reconnects": harness.reconnects,
            "steps": [{"name": "long-soak", "status": "passed"}],
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        eprintln!("R09 report {}", path.to_string_lossy());
    }
}
