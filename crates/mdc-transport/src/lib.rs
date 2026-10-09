mod serial_policy;

pub use serial_policy::{
    is_candidate_serial_port, serial_open_failure_backoff_ms, SerialOpenGuard,
    FOREIGN_DENYLIST_COOLDOWN_MS, FOREIGN_MAX_PROBE_FAILURES, POST_WRITE_RESET_SEQUENCE,
    SERIAL_OPEN_FAIL_ESCALATION_THRESHOLD, SERIAL_OPEN_PERMANENT_BLOCK_MS,
    SERIAL_TRANSIENT_MAX_BACKOFF_MS,
};

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;

/// Maximum queued writes on MemoryLink before Backpressure.
pub const MEMORY_LINK_MAX_QUEUE: usize = 64;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TransportError {
    #[error("transport is closed")]
    Closed,
    #[error("write queue is full")]
    Backpressure,
    #[error("I/O error: {0}")]
    Io(String),
    #[error("invalid endpoint: {0}")]
    Endpoint(String),
}

/// Byte channel. `close` cancels the link; `cancel` is an alias (default impl).
pub trait Transport: Send {
    fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError>;
    fn read(&mut self) -> Result<Option<Vec<u8>>, TransportError>;
    fn close(&mut self);
    /// Alias for [`close`](Transport::close); cancels outstanding I/O.
    fn cancel(&mut self) {
        self.close();
    }
}

#[derive(Clone, Default)]
pub struct MemoryLink {
    incoming: Arc<Mutex<VecDeque<Vec<u8>>>>,
    outgoing: Arc<Mutex<VecDeque<Vec<u8>>>>,
    closed: Arc<Mutex<bool>>,
}
impl MemoryLink {
    pub fn pair() -> (Self, Self) {
        let a = Self::default();
        let b = Self {
            incoming: a.outgoing.clone(),
            outgoing: a.incoming.clone(),
            closed: a.closed.clone(),
        };
        (a, b)
    }
}
impl Transport for MemoryLink {
    fn write(&mut self, b: &[u8]) -> Result<(), TransportError> {
        if *self.closed.lock().unwrap() {
            return Err(TransportError::Closed);
        }
        let mut q = self.outgoing.lock().unwrap();
        if q.len() >= MEMORY_LINK_MAX_QUEUE {
            return Err(TransportError::Backpressure);
        }
        q.push_back(b.to_vec());
        Ok(())
    }
    fn read(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        if *self.closed.lock().unwrap() {
            return Err(TransportError::Closed);
        }
        Ok(self.incoming.lock().unwrap().pop_front())
    }
    fn close(&mut self) {
        *self.closed.lock().unwrap() = true;
    }
}

/// Std TCP stream transport for simulator processes (no extra deps).
pub struct TcpTransport {
    stream: Option<TcpStream>,
}
impl TcpTransport {
    pub fn connect<A: ToSocketAddrs>(addr: A, timeout: Duration) -> Result<Self, TransportError> {
        let address = addr
            .to_socket_addrs()
            .map_err(|e| TransportError::Io(e.to_string()))?
            .next()
            .ok_or_else(|| TransportError::Endpoint("host has no addresses".into()))?;
        let stream = TcpStream::connect_timeout(&address, timeout)
            .map_err(|e| TransportError::Io(e.to_string()))?;
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|e| TransportError::Io(e.to_string()))?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|e| TransportError::Io(e.to_string()))?;
        stream
            .set_nodelay(true)
            .map_err(|e| TransportError::Io(e.to_string()))?;
        Ok(Self {
            stream: Some(stream),
        })
    }
    pub fn from_stream(stream: TcpStream) -> Result<Self, TransportError> {
        stream
            .set_nonblocking(false)
            .map_err(|e| TransportError::Io(e.to_string()))?;
        Ok(Self {
            stream: Some(stream),
        })
    }
}
impl Transport for TcpTransport {
    fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        let stream = self.stream.as_mut().ok_or(TransportError::Closed)?;
        stream
            .write_all(bytes)
            .map_err(|e| TransportError::Io(e.to_string()))
    }
    fn read(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        let stream = self.stream.as_mut().ok_or(TransportError::Closed)?;
        let mut buffer = [0u8; 16 * 1024];
        match stream.read(&mut buffer) {
            Ok(0) => Err(TransportError::Closed),
            Ok(n) => Ok(Some(buffer[..n].to_vec())),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                Ok(None)
            }
            Err(e) => Err(TransportError::Io(e.to_string())),
        }
    }
    fn close(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

#[cfg(feature = "serial")]
pub struct SerialTransport {
    port: Box<dyn serialport::SerialPort>,
}
#[cfg(feature = "serial")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SerialConfig {
    pub baud: u32,
    pub timeout: std::time::Duration,
    pub reset_on_open: bool,
}
#[cfg(feature = "serial")]
impl Default for SerialConfig {
    fn default() -> Self {
        Self {
            baud: 115_200,
            timeout: std::time::Duration::from_millis(100),
            reset_on_open: false,
        }
    }
}
#[cfg(feature = "serial")]
impl SerialTransport {
    pub fn open(
        path: &str,
        baud: u32,
        timeout: std::time::Duration,
    ) -> Result<Self, TransportError> {
        Self::open_with_config(
            path,
            SerialConfig {
                baud,
                timeout,
                reset_on_open: false,
            },
        )
    }
    pub fn open_with_config(path: &str, config: SerialConfig) -> Result<Self, TransportError> {
        let mut port = serialport::new(path, config.baud)
            .timeout(config.timeout)
            .open()
            .map_err(|e| TransportError::Io(e.to_string()))?;
        if config.reset_on_open {
            port.write_request_to_send(false)
                .map_err(|e| TransportError::Io(e.to_string()))?;
            port.write_data_terminal_ready(false)
                .map_err(|e| TransportError::Io(e.to_string()))?;
        }
        Ok(Self { port })
    }
}
#[cfg(feature = "serial")]
impl Transport for SerialTransport {
    fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.port
            .write_all(bytes)
            .map_err(|e| TransportError::Io(e.to_string()))
    }
    fn read(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        let mut buffer = [0u8; 16 * 1024];
        match self.port.read(&mut buffer) {
            Ok(0) => Ok(None),
            Ok(n) => Ok(Some(buffer[..n].to_vec())),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(None),
            Err(e) => Err(TransportError::Io(e.to_string())),
        }
    }
    fn close(&mut self) {
        let _ = self.port.clear(serialport::ClearBuffer::All);
    }
}

#[cfg(feature = "websocket")]
pub struct WebSocketTransport {
    socket: tungstenite::WebSocket<std::net::TcpStream>,
}
#[cfg(feature = "websocket")]
impl WebSocketTransport {
    pub fn connect(url: &str, timeout: std::time::Duration) -> Result<Self, TransportError> {
        let parsed = url::Url::parse(url).map_err(|e| TransportError::Endpoint(e.to_string()))?;
        let host = parsed
            .host()
            .ok_or_else(|| TransportError::Endpoint("missing host".into()))?;
        let port = parsed.port().unwrap_or(80);
        let address = (host.to_string(), port)
            .to_socket_addrs()
            .map_err(|e| TransportError::Io(e.to_string()))?
            .next()
            .ok_or_else(|| TransportError::Endpoint("host has no addresses".into()))?;
        let stream = std::net::TcpStream::connect_timeout(&address, timeout)
            .map_err(|e| TransportError::Io(e.to_string()))?;
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|e| TransportError::Io(e.to_string()))?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|e| TransportError::Io(e.to_string()))?;
        let (socket, _) =
            tungstenite::client(parsed, stream).map_err(|e| TransportError::Io(e.to_string()))?;
        Ok(Self { socket })
    }
}
#[cfg(feature = "websocket")]
impl Transport for WebSocketTransport {
    fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.socket
            .send(tungstenite::Message::Binary(bytes.to_vec()))
            .map_err(|e| TransportError::Io(e.to_string()))
    }
    fn read(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        match self.socket.read() {
            Ok(tungstenite::Message::Binary(bytes)) => Ok(Some(bytes)),
            Ok(tungstenite::Message::Ping(bytes)) => {
                self.socket
                    .send(tungstenite::Message::Pong(bytes))
                    .map_err(|e| TransportError::Io(e.to_string()))?;
                Ok(None)
            }
            Ok(tungstenite::Message::Pong(_)) => Ok(None),
            Ok(tungstenite::Message::Close(_)) => Err(TransportError::Closed),
            Ok(_) => Err(TransportError::Io(
                "text WebSocket messages are not supported".into(),
            )),
            Err(tungstenite::Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                Ok(None)
            }
            Err(e) => Err(TransportError::Io(e.to_string())),
        }
    }
    fn close(&mut self) {
        let _ = self.socket.close(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn memory_pair() {
        let (a, mut b) = MemoryLink::pair();
        let mut a = a;
        a.write(&[1, 2]).unwrap();
        assert_eq!(b.read().unwrap(), Some(vec![1, 2]));
        b.close();
        assert_eq!(a.write(&[3]), Err(TransportError::Closed));
    }

    #[test]
    fn memory_backpressure_when_queue_full() {
        let (mut a, _b) = MemoryLink::pair();
        for i in 0..MEMORY_LINK_MAX_QUEUE {
            a.write(&[i as u8]).unwrap();
        }
        assert_eq!(a.write(&[0xff]), Err(TransportError::Backpressure));
    }

    #[test]
    fn close_is_idempotent_and_write_after_close_fails() {
        let (mut a, mut b) = MemoryLink::pair();
        a.close();
        a.close(); // idempotent
        a.cancel(); // alias
        assert_eq!(a.write(&[1]), Err(TransportError::Closed));
        assert_eq!(b.write(&[2]), Err(TransportError::Closed));
        assert_eq!(a.read(), Err(TransportError::Closed));
    }

    #[test]
    fn tcp_transport_round_trip_and_close_contract() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4];
            stream.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, &[9, 8, 7, 6]);
            stream.write_all(&[1, 2, 3]).unwrap();
        });
        let mut client = TcpTransport::connect(addr, Duration::from_secs(2)).unwrap();
        client.write(&[9, 8, 7, 6]).unwrap();
        let reply = loop {
            if let Some(bytes) = client.read().unwrap() {
                break bytes;
            }
        };
        assert_eq!(reply, vec![1, 2, 3]);
        client.close();
        client.close(); // idempotent
        assert_eq!(client.write(&[0]), Err(TransportError::Closed));
        server.join().unwrap();
    }
}
