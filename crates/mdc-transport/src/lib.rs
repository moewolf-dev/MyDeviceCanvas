use std::collections::VecDeque;
#[cfg(any(feature = "serial", feature = "websocket"))]
use std::io::{Read, Write};
#[cfg(feature = "websocket")]
use std::net::ToSocketAddrs;
use std::sync::{Arc, Mutex};
use thiserror::Error;

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
pub trait Transport: Send {
    fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError>;
    fn read(&mut self) -> Result<Option<Vec<u8>>, TransportError>;
    fn close(&mut self);
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
        self.outgoing.lock().unwrap().push_back(b.to_vec());
        Ok(())
    }
    fn read(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        Ok(self.incoming.lock().unwrap().pop_front())
    }
    fn close(&mut self) {
        *self.closed.lock().unwrap() = true;
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
    #[test]
    fn memory_pair() {
        let (a, mut b) = MemoryLink::pair();
        let mut a = a;
        a.write(&[1, 2]).unwrap();
        assert_eq!(b.read().unwrap(), Some(vec![1, 2]));
        b.close();
        assert_eq!(a.write(&[3]), Err(TransportError::Closed));
    }
}
