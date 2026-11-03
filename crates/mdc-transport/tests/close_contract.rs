//! SIM-R06: one close/cancel contract for every byte channel.
//!
//! Memory, TCP and WebSocket links run the same assertions here. Serial runs
//! them over a real OS pty on unix, which also proves that `close` releases the
//! device handle instead of only clearing buffers.

use mdc_transport::{MemoryLink, TcpTransport, Transport, TransportError};
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};

/// A transport must survive repeated close/cancel, settle within the 500 ms
/// cancellation window and reject all I/O afterwards.
fn assert_close_contract(mut transport: impl Transport, label: &str) {
    let started = Instant::now();
    transport.close();
    transport.close();
    transport.cancel();
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "{label}: close/cancel took {:?}",
        started.elapsed()
    );
    assert_eq!(
        transport.write(&[1, 2, 3]),
        Err(TransportError::Closed),
        "{label}: write after close must fail"
    );
    assert_eq!(
        transport.read(),
        Err(TransportError::Closed),
        "{label}: read after close must fail"
    );
}

/// An idle read must return `None` (or an error) within a bounded window; it
/// must never park the caller indefinitely.
fn assert_idle_read_is_bounded(transport: &mut impl Transport, label: &str, budget: Duration) {
    let started = Instant::now();
    let result = transport.read();
    let elapsed = started.elapsed();
    assert!(
        elapsed < budget,
        "{label}: idle read took {elapsed:?}, budget {budget:?}"
    );
    assert!(result.is_ok(), "{label}: idle read failed: {result:?}");
}

#[test]
fn memory_close_is_idempotent_and_rejects_io() {
    let (mut link, _peer) = MemoryLink::pair();
    assert_idle_read_is_bounded(&mut link, "memory", Duration::from_millis(50));
    assert_close_contract(link, "memory");
}

#[test]
fn tcp_close_is_idempotent_and_rejects_io() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        thread::sleep(Duration::from_millis(400));
        drop(stream);
    });
    let mut client = TcpTransport::connect(addr, Duration::from_millis(200)).unwrap();
    assert_idle_read_is_bounded(&mut client, "tcp", Duration::from_millis(500));
    assert_close_contract(client, "tcp");
    server.join().unwrap();
}

#[cfg(feature = "websocket")]
#[test]
fn websocket_close_is_idempotent_and_rejects_io_across_the_matrix() {
    use mdc_transport::WebSocketTransport;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut peer = WebSocketTransport::accept(stream).unwrap();
        let _ = peer.read();
        thread::sleep(Duration::from_millis(100));
    });
    let client = WebSocketTransport::connect(
        &format!("ws://127.0.0.1:{port}/"),
        Duration::from_millis(200),
    )
    .unwrap();
    let mut client = client;
    assert_idle_read_is_bounded(&mut client, "websocket", Duration::from_millis(500));
    assert_close_contract(client, "websocket");
    let _ = server.join();
}

#[test]
fn short_write_resumes_without_duplicating_the_prefix() {
    use mdc_transport::ShortWriteLink;
    let (inner, mut peer) = MemoryLink::pair();
    let mut link = ShortWriteLink::new(inner);
    link.arm(3);
    let payload = [1u8, 2, 3, 4, 5, 6];
    assert_eq!(
        link.write(&payload),
        Err(TransportError::ShortWrite { accepted: 3 })
    );
    // The caller resends only the unaccepted tail.
    link.write(&payload[3..]).unwrap();
    let mut received = Vec::new();
    while let Some(bytes) = peer.read().unwrap() {
        received.extend_from_slice(&bytes);
    }
    assert_eq!(received, payload.to_vec());
}

#[cfg(all(feature = "serial", unix))]
mod serial_pty {
    use super::*;
    use mdc_transport::{SerialConfig, SerialTransport};
    use std::ffi::CStr;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::unix::io::{AsRawFd, FromRawFd};

    struct Pty {
        master: File,
        path: String,
    }

    fn open_pty() -> Pty {
        unsafe {
            let mut master: libc::c_int = -1;
            let mut slave: libc::c_int = -1;
            let mut name = [0 as libc::c_char; 1024];
            let rc = libc::openpty(
                &mut master,
                &mut slave,
                name.as_mut_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            assert_eq!(rc, 0, "openpty failed");
            // The pathname is all we need; the transport opens the slave itself.
            libc::close(slave);
            let path = CStr::from_ptr(name.as_ptr()).to_string_lossy().into_owned();
            let master = File::from_raw_fd(master);
            let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
            assert!(flags >= 0);
            assert_eq!(
                libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK),
                0
            );
            Pty { master, path }
        }
    }

    fn read_exact_with_deadline(master: &mut File, expected: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut got = Vec::new();
        while got.len() < expected.len() {
            assert!(Instant::now() < deadline, "timed out, got {got:?}");
            let mut buffer = [0u8; 64];
            match master.read(&mut buffer) {
                Ok(0) => thread::sleep(Duration::from_millis(1)),
                Ok(n) => got.extend_from_slice(&buffer[..n]),
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(1))
                }
                Err(err) => panic!("master read failed: {err}"),
            }
        }
        assert_eq!(got, expected);
    }

    #[test]
    fn serial_pty_round_trip_close_contract_and_handle_release() {
        let mut pty = open_pty();
        let config = SerialConfig {
            // PTYs reject the baud ioctl; the adapter falls back internally.
            baud: 115_200,
            timeout: Duration::from_millis(100),
            reset_on_open: false,
        };
        let mut transport = SerialTransport::open_with_config(&pty.path, config).unwrap();
        assert!(transport.is_open());
        assert_idle_read_is_bounded(&mut transport, "serial-pty", Duration::from_millis(500));

        // Device -> host.
        pty.master.write_all(&[7, 8, 9]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let received = loop {
            match transport.read().unwrap() {
                Some(bytes) => break bytes,
                None => {
                    assert!(Instant::now() < deadline, "no pty bytes reached the host");
                }
            }
        };
        assert_eq!(received, vec![7, 8, 9]);

        // Host -> device.
        transport.write(&[4, 5, 6]).unwrap();
        read_exact_with_deadline(&mut pty.master, &[4, 5, 6]);

        assert_close_contract(transport, "serial-pty");

        // Closing must release the OS handle: reopening the same slave path
        // would fail with "resource busy" if the old handle were still held.
        let reopened = SerialTransport::open_with_config(&pty.path, config).unwrap();
        assert!(reopened.is_open());
        drop(reopened);

        let mut reopened = SerialTransport::open_with_config(&pty.path, config).unwrap();
        reopened.close();
        reopened.close();
        assert!(!reopened.is_open());
    }
}
