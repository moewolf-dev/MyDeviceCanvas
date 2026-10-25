//! SIM-R08: cross-process fault matrix.
//!
//! Each test spawns the real `mdc-sim-peer` binary and drives it through the
//! public Rust Core `Session`, so transport and process boundaries are real.
use mdc_core::{ConnectionState, CoreError, Frame, Session, SessionConfig};
use mdc_transport::{TcpTransport, Transport, WebSocketTransport};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Peer {
    child: Child,
    addr: String,
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn reserve_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve port");
    listener.local_addr().unwrap().port()
}

fn spawn_peer(flags: &[&str]) -> Peer {
    spawn_peer_board("esp32-jc3248w535-sim", flags)
}

fn spawn_peer_board(board: &str, flags: &[&str]) -> Peer {
    let port = reserve_port();
    let addr = format!("127.0.0.1:{port}");
    let mut command = Command::new(env!("CARGO_BIN_EXE_mdc-sim-peer"));
    command
        .arg(&addr)
        .arg(board)
        .args(flags)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = command.spawn().expect("spawn mdc-sim-peer");
    Peer { child, addr }
}

fn config(ack_timeout_ms: u64, heartbeat_ms: u64) -> SessionConfig {
    SessionConfig {
        connect_timeout_ms: 1_000,
        ack_timeout_ms,
        heartbeat_ms,
        reconnect: false,
    }
}

fn connect_tcp(addr: &str, config: SessionConfig) -> Session<TcpTransport> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match TcpTransport::connect(addr, Duration::from_millis(300)) {
            Ok(transport) => {
                let mut session = Session::with_config(transport, config);
                session.connect().expect("send HELLO");
                return session;
            }
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("connect {addr}: {error}"),
        }
    }
}

fn connect_ws(addr: &str, config: SessionConfig) -> Session<WebSocketTransport> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let url = format!("ws://{addr}/");
        match WebSocketTransport::connect(&url, Duration::from_millis(300)) {
            Ok(transport) => {
                let mut session = Session::with_config(transport, config);
                session.connect().expect("send HELLO");
                return session;
            }
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("connect {url}: {error}"),
        }
    }
}

fn wait_ready<T: Transport>(session: &mut Session<T>) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        match session.poll() {
            Ok(ConnectionState::Ready) => return,
            Ok(_) => {}
            Err(error) => panic!("handshake failed: {error}"),
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("session did not become ready: {:?}", session.state);
}

fn full_frame() -> Frame {
    Frame {
        surface_id: "main".into(),
        width: 480,
        height: 320,
        bytes: vec![0x33; 480 * 320 * 2],
    }
}

fn wait_ack<T: Transport>(session: &mut Session<T>) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let _ = session.poll();
        if session.stats().acks_received > 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("frame was not acknowledged");
}

fn poll_until_error<T: Transport>(session: &mut Session<T>) -> Option<CoreError> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        match session.poll() {
            Ok(_) => std::thread::sleep(Duration::from_millis(5)),
            Err(error) => return Some(error),
        }
    }
    None
}

#[test]
fn lost_request_times_out_without_a_false_display() {
    // The peer silently drops the frame request, so no ACK ever arrives.
    let peer = spawn_peer(&["--lose-after", "1"]);
    let mut session = connect_tcp(&peer.addr, config(200, 0));
    wait_ready(&mut session);
    session.send_frame(full_frame()).expect("send frame");
    assert_eq!(session.tick(250), Err(CoreError::AckTimeout));
    assert_ne!(session.state, ConnectionState::Ready);
    assert_eq!(session.stats().acks_received, 0);
}

#[test]
fn delayed_processing_still_completes_the_handshake_and_frame() {
    let peer = spawn_peer(&["--delay-ticks", "5"]);
    let mut session = connect_tcp(&peer.addr, config(1_000, 0));
    wait_ready(&mut session);
    session.send_frame(full_frame()).expect("send frame");
    wait_ack(&mut session);
    assert_eq!(session.stats().acks_received, 1);
}

#[test]
fn corrupted_ack_is_rejected_not_displayed() {
    let peer = spawn_peer(&["--corrupt-after", "1"]);
    let mut session = connect_tcp(&peer.addr, config(0, 0));
    wait_ready(&mut session);
    session.send_frame(full_frame()).expect("send frame");
    let error = poll_until_error(&mut session).expect("corrupt ACK must be rejected");
    assert!(matches!(error, CoreError::Protocol(_)), "{error:?}");
    assert_eq!(session.stats().acks_received, 0);
}

#[test]
fn peer_exit_converges_away_from_ready() {
    let peer = spawn_peer(&["--exit-after-ms", "600"]);
    let mut session = connect_tcp(&peer.addr, config(0, 0));
    wait_ready(&mut session);
    std::thread::sleep(Duration::from_millis(800));
    let error = poll_until_error(&mut session).expect("closed peer must surface");
    assert!(matches!(error, CoreError::Transport(_)), "{error:?}");
    assert_ne!(session.state, ConnectionState::Ready);
}

#[test]
fn stalled_peer_times_out_on_ack() {
    let peer = spawn_peer(&["--stall-after-ms", "250"]);
    let mut session = connect_tcp(&peer.addr, config(200, 0));
    wait_ready(&mut session);
    std::thread::sleep(Duration::from_millis(350));
    session.send_frame(full_frame()).expect("send frame");
    assert_eq!(session.tick(250), Err(CoreError::AckTimeout));
    assert_ne!(session.state, ConnectionState::Ready);
    assert_eq!(session.stats().acks_received, 0);
}

#[test]
fn reboot_updates_run_identity_and_recovers() {
    let peer = spawn_peer(&["--reboot-after-ms", "1200"]);
    let mut session = connect_tcp(&peer.addr, config(1_000, 0));
    wait_ready(&mut session);
    let before_boot = session.device_boot_id();
    let before_run = session.device_run_id();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && session.device_run_id() == before_run {
        let _ = session.poll();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_ne!(session.device_run_id(), before_run);
    assert_ne!(session.device_boot_id(), before_boot);
    assert_eq!(session.state, ConnectionState::Ready);
    session.send_frame(full_frame()).expect("send frame");
    wait_ack(&mut session);
}

#[test]
fn duplicate_ack_is_idempotent() {
    let peer = spawn_peer(&["--duplicate-after", "1"]);
    let mut session = connect_tcp(&peer.addr, config(1_000, 0));
    wait_ready(&mut session);
    session.send_frame(full_frame()).expect("send frame");
    wait_ack(&mut session);
    std::thread::sleep(Duration::from_millis(120));
    let _ = session.poll();
    assert_eq!(session.stats().acks_received, 1);
    assert_eq!(session.displayed_timings().len(), 1);
}

#[test]
fn split_outbound_packets_still_round_trip() {
    // The peer writes one packet in two transport writes; Core must reassemble.
    let peer = spawn_peer(&["--split-after", "1"]);
    let mut session = connect_tcp(&peer.addr, config(1_000, 0));
    wait_ready(&mut session);
    session.send_frame(full_frame()).expect("send frame");
    wait_ack(&mut session);
    assert_eq!(session.stats().acks_received, 1);
}

#[test]
fn out_of_order_acks_release_both_surfaces() {
    // Two frames on the dual-surface board: the peer emits their ACKs in
    // reverse order. Both requests must still complete exactly once.
    let peer = spawn_peer_board("dual-sim", &["--swap-next-acks"]);
    let mut session = connect_tcp(&peer.addr, config(1_000, 0));
    wait_ready(&mut session);
    let surfaces: Vec<(String, u16, u16)> = session
        .device
        .as_ref()
        .unwrap()
        .capabilities
        .surfaces
        .iter()
        .map(|surface| (surface.id.clone(), surface.width, surface.height))
        .collect();
    for (id, width, height) in &surfaces {
        session
            .send_frame(Frame {
                surface_id: id.clone(),
                width: *width,
                height: *height,
                bytes: vec![0x5a; usize::from(*width) * usize::from(*height) * 2],
            })
            .expect("send frame");
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && session.stats().acks_received < surfaces.len() as u64 {
        let _ = session.poll();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(session.stats().acks_received, surfaces.len() as u64);
    assert_eq!(session.pending_request_count(), 0);
}

#[test]
fn split_all_writes_round_trip_a_full_frame() {
    // Every outbound packet is written in two pieces, including the 307 KiB
    // frame ACK path; Core must reassemble across transport reads.
    let peer = spawn_peer(&["--split-all"]);
    let mut session = connect_tcp(&peer.addr, config(1_000, 0));
    wait_ready(&mut session);
    session.send_frame(full_frame()).expect("send frame");
    wait_ack(&mut session);
    assert_eq!(session.stats().acks_received, 1);
}

#[test]
fn mid_link_capability_change_updates_the_session() {
    let peer = spawn_peer(&["--recaps-after-ms", "600", "--recaps-fps", "7"]);
    let mut session = connect_tcp(&peer.addr, config(0, 0));
    wait_ready(&mut session);
    let before = session
        .device
        .as_ref()
        .unwrap()
        .capabilities
        .local_display_fps;
    assert_eq!(before, 30);
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let _ = session.poll();
        let fps = session
            .device
            .as_ref()
            .map(|device| device.capabilities.local_display_fps);
        if fps == Some(7) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        session
            .device
            .as_ref()
            .unwrap()
            .capabilities
            .local_display_fps,
        7
    );
    assert_eq!(session.state, ConnectionState::Ready);
    session.send_frame(full_frame()).expect("send frame");
    wait_ack(&mut session);
}

#[test]
fn websocket_peer_round_trips_a_frame() {
    let peer = spawn_peer(&["--ws"]);
    let mut session = connect_ws(&peer.addr, config(1_000, 0));
    wait_ready(&mut session);
    session.send_frame(full_frame()).expect("send frame");
    wait_ack(&mut session);
}

#[test]
fn one_device_exiting_does_not_close_another() {
    let peer_a = spawn_peer(&["--exit-after-ms", "500"]);
    let peer_b = spawn_peer(&[]);
    let mut a = connect_tcp(&peer_a.addr, config(0, 0));
    let mut b = connect_tcp(&peer_b.addr, config(1_000, 0));
    wait_ready(&mut a);
    wait_ready(&mut b);
    std::thread::sleep(Duration::from_millis(700));
    let error = poll_until_error(&mut a).expect("A must surface its close");
    assert!(matches!(error, CoreError::Transport(_)), "{error:?}");
    assert_eq!(b.state, ConnectionState::Ready);
    b.send_frame(full_frame()).expect("B still works");
    wait_ack(&mut b);
    assert_eq!(b.stats().acks_received, 1);
}
