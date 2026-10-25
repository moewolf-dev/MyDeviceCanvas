//! SIM-R08: C++ Linux runtime in its own process, driven by a Rust host.
//!
//! Build the peer first and point `MDC_CXX_PEER_BIN` at it:
//! `c++ ... runtime/linux/tests/cxx_peer_main.cpp -o /tmp/mdc-cxx-peer`
//! Without the variable these tests skip, so the default workspace test run does
//! not require a C++ toolchain.
use mdc_core::{ConnectionState, CoreError, Frame, Session, SessionConfig, Tile};
use mdc_transport::{TcpTransport, Transport, TransportError};
use std::net::TcpListener;
use std::path::PathBuf;
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

fn spawn_peer(width: u16, height: u16) -> Option<Peer> {
    spawn_peer_with_dump(width, height, None)
}

fn spawn_peer_with_dump(width: u16, height: u16, dump: Option<PathBuf>) -> Option<Peer> {
    let binary = std::env::var_os("MDC_CXX_PEER_BIN")?;
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve port");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let addr = format!("127.0.0.1:{port}");
    let mut command = Command::new(binary);
    command
        .arg(&addr)
        .arg(width.to_string())
        .arg(height.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(path) = dump {
        command.env("MDC_CXX_PEER_DUMP", path);
    }
    let child = command.spawn().expect("spawn cxx peer");
    Some(Peer { child, addr })
}

fn config() -> SessionConfig {
    SessionConfig {
        connect_timeout_ms: 1_000,
        ack_timeout_ms: 2_000,
        heartbeat_ms: 0,
        reconnect: false,
    }
}

fn connect(addr: &str) -> Session<TcpTransport> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match TcpTransport::connect(addr, Duration::from_millis(300)) {
            Ok(transport) => {
                let mut session = Session::with_config(transport, config());
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

fn wait_acks<T: Transport>(session: &mut Session<T>, count: u64) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let _ = session.poll();
        if session.stats().acks_received >= count {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!(
        "expected {count} ACKs, got {}",
        session.stats().acks_received
    );
}

fn big_frame() -> Frame {
    Frame {
        surface_id: "main".into(),
        width: 200,
        height: 100,
        bytes: vec![0x42; 200 * 100 * 2],
    }
}

#[test]
fn cxx_runtime_reassembles_a_chunked_frame_and_tile() {
    let dump = std::env::temp_dir().join(format!("mdc-cxx-dump-{}.bin", std::process::id()));
    let _ = std::fs::remove_file(&dump);
    let Some(peer) = spawn_peer_with_dump(200, 100, Some(dump.clone())) else {
        eprintln!("MDC_CXX_PEER_BIN is unset; skipping C++ peer test");
        return;
    };
    let mut session = connect(&peer.addr);
    wait_ready(&mut session);
    session.send_frame(big_frame()).expect("send frame");
    wait_acks(&mut session, 1);
    assert_eq!(session.pending_request_count(), 0);
    session
        .send_tile(Tile {
            surface_id: "main".into(),
            base_frame_id: 1,
            x: 0,
            y: 0,
            width: 4,
            height: 2,
            bytes: vec![0x77; 4 * 2 * 2],
        })
        .expect("send tile");
    wait_acks(&mut session, 2);
    assert_eq!(session.pending_request_count(), 0);
    assert_eq!(session.state, ConnectionState::Ready);
    let expected_len = 200 * 100 * 2;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut pixels = Vec::new();
    while Instant::now() < deadline {
        if let Ok(bytes) = std::fs::read(&dump) {
            if bytes.len() == expected_len && bytes[0] == 0x77 {
                pixels = bytes;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = std::fs::remove_file(&dump);
    assert_eq!(pixels.len(), expected_len);
    // The 4x2 tile at the origin is 0x77; the rest of the frame stays 0x42.
    // Each 4-pixel row is 8 bytes. Row 0 starts at byte 0, row 1 at
    // (y=1 * width=200) * 2 = 400.
    assert!(pixels[..8].iter().all(|byte| *byte == 0x77));
    assert!(pixels[400..408].iter().all(|byte| *byte == 0x77));
    assert!(pixels[8..400].iter().all(|byte| *byte == 0x42));
    assert!(pixels[408..416].iter().all(|byte| *byte == 0x42));
    assert!(pixels[416..].iter().all(|byte| *byte == 0x42));
}

struct ShortOnce {
    inner: TcpTransport,
    armed: bool,
}

impl ShortOnce {
    fn new(inner: TcpTransport) -> Self {
        Self {
            inner,
            armed: false,
        }
    }
}

impl Transport for ShortOnce {
    fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        if self.armed {
            self.armed = false;
            let accepted = (bytes.len() / 2).max(1).min(bytes.len());
            self.inner.write(&bytes[..accepted])?;
            return Err(TransportError::ShortWrite { accepted });
        }
        self.inner.write(bytes)
    }
    fn read(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        self.inner.read()
    }
    fn close(&mut self) {
        self.inner.close();
    }
}

#[test]
fn cxx_runtime_resumes_after_a_short_begin_write() {
    let Some(peer) = spawn_peer(200, 100) else {
        eprintln!("MDC_CXX_PEER_BIN is unset; skipping C++ peer test");
        return;
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    let transport = loop {
        match TcpTransport::connect(&peer.addr, Duration::from_millis(300)) {
            Ok(transport) => break ShortOnce::new(transport),
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("connect {}: {error}", peer.addr),
        }
    };
    let mut session = Session::with_config(transport, config());
    session.connect().expect("send HELLO");
    wait_ready(&mut session);
    session.transport_mut().armed = true;
    assert_eq!(session.send_frame(big_frame()), Err(CoreError::ShortWrite));
    session.resume_write().expect("resume BEGIN tail");
    wait_acks(&mut session, 1);
    assert_eq!(session.pending_request_count(), 0);
}

#[test]
fn cxx_peer_exit_converges_away_from_ready() {
    let Some(peer) = spawn_peer(8, 4) else {
        eprintln!("MDC_CXX_PEER_BIN is unset; skipping C++ peer test");
        return;
    };
    let mut session = connect(&peer.addr);
    wait_ready(&mut session);
    let mut peer = peer;
    let _ = peer.child.kill();
    let _ = peer.child.wait();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut failure = None;
    while Instant::now() < deadline {
        match session.poll() {
            Ok(_) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    let failure = failure.expect("peer close must surface");
    assert!(matches!(failure, CoreError::Transport(_)), "{failure:?}");
    assert_ne!(session.state, ConnectionState::Ready);
}
