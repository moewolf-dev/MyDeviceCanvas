//! Real mDNS / DNS-SD discovery (N16).
//!
//! A minimal, dependency-free subset: it encodes a PTR query for a service type,
//! parses PTR/SRV/TXT/A answers (including DNS name compression), and turns the
//! records into `Endpoint::Tcp` candidates. Identity is still confirmed by HELLO
//! — discovery only offers endpoints.
//!
//! The tests build a response with [`build_response`] (a device-side responder
//! shape) and also round-trip it over a loopback UDP socket, so the parser and
//! provider are exercised without touching real multicast.
use crate::{Candidate, DiscoveryProvider, Endpoint};
use std::net::Ipv4Addr;
use thiserror::Error;

/// Service type MDC devices advertise.
pub const DEFAULT_SERVICE: &str = "_mdc._tcp.local";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MdnsError {
    #[error("mdns message truncated")]
    Truncated,
    #[error("mdns name is malformed")]
    Name,
    #[error("mdns compression pointer loop")]
    Loop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordKind {
    Ptr(String),
    Srv { port: u16, target: String },
    Txt(Vec<String>),
    A(Ipv4Addr),
    Other(u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdnsRecord {
    pub name: String,
    pub kind: RecordKind,
}

/// Encode one DNS-SD PTR query for `service`.
pub fn encode_query(service: &str) -> Vec<u8> {
    let mut out = vec![0u8; 12];
    out[5] = 1; // QDCOUNT = 1
    encode_name(service, &mut out);
    out.extend_from_slice(&12u16.to_be_bytes()); // QTYPE PTR
    out.extend_from_slice(&1u16.to_be_bytes()); // QCLASS IN
    out
}

/// Build a DNS-SD response with PTR (service→instance), SRV (instance→host:port)
/// and A (host→IPv4) records. This is the shape a device responder returns and
/// what the tests feed the parser.
pub fn build_response(
    service: &str,
    instance: &str,
    host: &str,
    port: u16,
    ip: Ipv4Addr,
) -> Vec<u8> {
    let mut out = vec![0u8; 12];
    out[2] = 0x84; // QR=1, AA=1
    out[7] = 3; // ANCOUNT = 3
                // PTR: service -> instance
    encode_name(service, &mut out);
    push_rr_header(&mut out, 12);
    let mut rdata = Vec::new();
    encode_name(instance, &mut rdata);
    push_rdata(&mut out, &rdata);
    // SRV: instance -> host:port
    encode_name(instance, &mut out);
    push_rr_header(&mut out, 33);
    let mut srv = Vec::new();
    srv.extend_from_slice(&0u16.to_be_bytes()); // priority
    srv.extend_from_slice(&0u16.to_be_bytes()); // weight
    srv.extend_from_slice(&port.to_be_bytes());
    encode_name(host, &mut srv);
    push_rdata(&mut out, &srv);
    // A: host -> ip
    encode_name(host, &mut out);
    push_rr_header(&mut out, 1);
    push_rdata(&mut out, &ip.octets());
    out
}

fn encode_name(name: &str, out: &mut Vec<u8>) {
    for label in name.split('.').filter(|l| !l.is_empty()) {
        let bytes = label.as_bytes();
        let len = bytes.len().min(63) as u8;
        out.push(len);
        out.extend_from_slice(&bytes[..usize::from(len)]);
    }
    out.push(0);
}

fn push_rr_header(out: &mut Vec<u8>, kind: u16) {
    out.extend_from_slice(&kind.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // CLASS IN
    out.extend_from_slice(&120u32.to_be_bytes()); // TTL
}

fn push_rdata(out: &mut Vec<u8>, rdata: &[u8]) {
    out.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
    out.extend_from_slice(rdata);
}

/// Parse a DNS message (header + questions skipped + answers).
pub fn parse_records(bytes: &[u8]) -> Result<Vec<MdnsRecord>, MdnsError> {
    if bytes.len() < 12 {
        return Err(MdnsError::Truncated);
    }
    let qd = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
    let an = u16::from_be_bytes([bytes[6], bytes[7]]) as usize;
    let mut offset = 12;
    for _ in 0..qd {
        let (_, next) = read_name(bytes, offset)?;
        offset = next + 4; // QTYPE + QCLASS
        if offset > bytes.len() {
            return Err(MdnsError::Truncated);
        }
    }
    let mut records = Vec::with_capacity(an);
    for _ in 0..an {
        let (name, next) = read_name(bytes, offset)?;
        let mut cursor = next;
        let kind = read_u16(bytes, cursor)?;
        cursor += 2; // type
        cursor += 2; // class
        cursor += 4; // ttl
        let rdlength = read_u16(bytes, cursor)? as usize;
        cursor += 2;
        let rdata = bytes
            .get(cursor..cursor + rdlength)
            .ok_or(MdnsError::Truncated)?;
        records.push(MdnsRecord {
            name,
            kind: parse_rdata(bytes, cursor, kind, rdata)?,
        });
        offset = cursor + rdlength;
    }
    Ok(records)
}

fn parse_rdata(
    message: &[u8],
    offset: usize,
    kind: u16,
    rdata: &[u8],
) -> Result<RecordKind, MdnsError> {
    Ok(match kind {
        1 if rdata.len() == 4 => {
            RecordKind::A(Ipv4Addr::new(rdata[0], rdata[1], rdata[2], rdata[3]))
        }
        12 => {
            let (target, _) = read_name(message, offset)?;
            RecordKind::Ptr(target)
        }
        16 => {
            let mut items = Vec::new();
            let mut i = 0;
            while i < rdata.len() {
                let len = rdata[i] as usize;
                if i + 1 + len > rdata.len() {
                    break;
                }
                items.push(String::from_utf8_lossy(&rdata[i + 1..i + 1 + len]).to_string());
                i += 1 + len;
            }
            RecordKind::Txt(items)
        }
        33 if rdata.len() >= 6 => {
            let port = u16::from_be_bytes([rdata[4], rdata[5]]);
            let (target, _) = read_name(message, offset + 6)?;
            RecordKind::Srv { port, target }
        }
        other => RecordKind::Other(other),
    })
}

/// Resolve a service type to TCP endpoints: PTR → instance → SRV → A.
pub fn service_endpoints(records: &[MdnsRecord], service: &str) -> Vec<Endpoint> {
    let instances: Vec<&str> = records
        .iter()
        .filter(|r| r.name == service)
        .filter_map(|r| match &r.kind {
            RecordKind::Ptr(instance) => Some(instance.as_str()),
            _ => None,
        })
        .collect();
    let mut out = Vec::new();
    for instance in instances {
        let Some((port, target)) = records.iter().find_map(|r| match &r.kind {
            RecordKind::Srv { port, target } if r.name == instance => Some((*port, target.clone())),
            _ => None,
        }) else {
            continue;
        };
        let Some(ip) = records.iter().find_map(|r| match &r.kind {
            RecordKind::A(ip) if r.name == target => Some(*ip),
            _ => None,
        }) else {
            continue;
        };
        let address = format!("{ip}:{port}");
        if !out.contains(&Endpoint::Tcp {
            address: address.clone(),
        }) {
            out.push(Endpoint::Tcp { address });
        }
    }
    out
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, MdnsError> {
    let slice = bytes.get(offset..offset + 2).ok_or(MdnsError::Truncated)?;
    Ok(u16::from_be_bytes([slice[0], slice[1]]))
}

/// Read a (possibly compressed) name and return it plus the offset just past the
/// first name segment.
fn read_name(bytes: &[u8], start: usize) -> Result<(String, usize), MdnsError> {
    let mut labels = Vec::new();
    let mut offset = start;
    let mut end: Option<usize> = None;
    let mut jumps = 0;
    loop {
        let len = *bytes.get(offset).ok_or(MdnsError::Truncated)?;
        if len & 0xC0 == 0xC0 {
            let next = *bytes.get(offset + 1).ok_or(MdnsError::Truncated)?;
            let pointer = (usize::from(len & 0x3F) << 8) | usize::from(next);
            end.get_or_insert(offset + 2);
            jumps += 1;
            if jumps > 16 {
                return Err(MdnsError::Loop);
            }
            offset = pointer;
            continue;
        }
        if len == 0 {
            let finish = end.unwrap_or(offset + 1);
            let name = if labels.is_empty() {
                ".".to_string()
            } else {
                labels.join(".")
            };
            return Ok((name, finish));
        }
        let label = bytes
            .get(offset + 1..offset + 1 + usize::from(len))
            .ok_or(MdnsError::Name)?;
        labels.push(String::from_utf8_lossy(label).to_string());
        offset += 1 + usize::from(len);
    }
}

/// DNS-SD discovery provider. Feed it response packets (from a real socket via
/// [`MdnsDiscovery::ingest`], or injected in tests).
#[derive(Debug, Clone)]
pub struct MdnsDiscovery {
    service: String,
    packets: Vec<Vec<u8>>,
    cancelled: bool,
}

impl Default for MdnsDiscovery {
    fn default() -> Self {
        Self::new(DEFAULT_SERVICE)
    }
}

impl MdnsDiscovery {
    pub fn new(service: &str) -> Self {
        Self {
            service: service.to_string(),
            packets: Vec::new(),
            cancelled: false,
        }
    }
    pub fn service(&self) -> &str {
        &self.service
    }
    /// The PTR query to send to `224.0.0.251:5353`.
    pub fn query_packet(&self) -> Vec<u8> {
        encode_query(&self.service)
    }
    /// Store a received response packet for the next `discover()`.
    pub fn ingest(&mut self, packet: Vec<u8>) {
        self.packets.push(packet);
    }

    /// Best-effort live query: send the PTR query to `224.0.0.251:5353` and
    /// collect responses for `timeout`. Multicast may be unavailable (sandbox,
    /// no route); that is not an error — it simply yields no candidates.
    pub fn query_live(&mut self, timeout: std::time::Duration) -> std::io::Result<usize> {
        use std::net::UdpSocket;
        let socket = UdpSocket::bind(("0.0.0.0", 0))?;
        let slice = timeout.min(std::time::Duration::from_millis(500));
        socket.set_read_timeout(Some(slice.max(std::time::Duration::from_millis(1))))?;
        let query = self.query_packet();
        let _ = socket.send_to(&query, ("224.0.0.251", 5353));
        let deadline = std::time::Instant::now() + timeout;
        let mut buffer = vec![0u8; 4096];
        let mut received = 0;
        while std::time::Instant::now() < deadline {
            match socket.recv_from(&mut buffer) {
                Ok((len, _)) => {
                    self.ingest(buffer[..len].to_vec());
                    received += 1;
                }
                Err(_) => break,
            }
        }
        Ok(received)
    }
    pub fn candidates_from(&self, packet: &[u8]) -> Vec<Candidate> {
        let Ok(records) = parse_records(packet) else {
            return Vec::new();
        };
        let endpoints = service_endpoints(&records, &self.service);
        if endpoints.is_empty() {
            Vec::new()
        } else {
            vec![Candidate {
                device_id: None,
                endpoints,
            }]
        }
    }
}

impl DiscoveryProvider for MdnsDiscovery {
    fn discover(&mut self) -> Vec<Candidate> {
        if self.cancelled {
            return Vec::new();
        }
        let mut out = Vec::new();
        for packet in &self.packets {
            out.extend(self.candidates_from(packet));
        }
        out
    }
    fn cancel(&mut self) {
        self.cancelled = true;
    }
}

/// Bind a loopback UDP responder that answers the MDC query once. The caller
/// sends the query; the responder replies with [`build_response`]. Returns the
/// bound address. This is a host-local stand-in for the multicast responder.
#[cfg(test)]
pub fn spawn_loopback_responder(port: u16, ip: Ipv4Addr) -> std::io::Result<std::net::UdpSocket> {
    let socket = std::net::UdpSocket::bind(("127.0.0.1", 0))?;
    let responder = socket.try_clone()?;
    std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        if let Ok((len, from)) = responder.recv_from(&mut buf) {
            let _ = len;
            let response = build_response(
                DEFAULT_SERVICE,
                "mdc-device._mdc._tcp.local",
                "mdc-device.local",
                port,
                ip,
            );
            let _ = responder.send_to(&response, from);
        }
    });
    Ok(socket)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_packet_has_one_ptr_question() {
        let q = encode_query(DEFAULT_SERVICE);
        assert_eq!(&q[0..6], &[0, 0, 0, 0, 0, 1]);
        // "_mdc" "_tcp" "local" then 0, then QTYPE 12, QCLASS 1.
        assert_eq!(&q[q.len() - 4..], &[0, 12, 0, 1]);
        assert_eq!(q.len(), 12 + 5 + 5 + 6 + 1 + 4);
    }

    #[test]
    fn response_parses_into_an_endpoint() {
        let packet = build_response(
            DEFAULT_SERVICE,
            "mdc-device._mdc._tcp.local",
            "mdc-device.local",
            8765,
            Ipv4Addr::new(192, 168, 1, 42),
        );
        let records = parse_records(&packet).unwrap();
        let endpoints = service_endpoints(&records, DEFAULT_SERVICE);
        assert_eq!(
            endpoints,
            vec![Endpoint::Tcp {
                address: "192.168.1.42:8765".into()
            }]
        );
    }

    #[test]
    fn provider_turns_injected_packets_into_a_candidate() {
        let packet = build_response(
            DEFAULT_SERVICE,
            "mdc-device._mdc._tcp.local",
            "mdc-device.local",
            9000,
            Ipv4Addr::new(10, 0, 0, 7),
        );
        let mut discovery = MdnsDiscovery::default();
        discovery.ingest(packet);
        let candidates = discovery.discover();
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].endpoints,
            vec![Endpoint::Tcp {
                address: "10.0.0.7:9000".into()
            }]
        );
        assert!(candidates[0].device_id.is_none(), "HELLO confirms identity");
        discovery.cancel();
        assert!(discovery.discover().is_empty());
    }

    #[test]
    fn name_compression_pointers_are_followed() {
        // "bar" + pointer to offset 0 where "foo" lives.
        let mut buf = Vec::new();
        buf.push(3);
        buf.extend_from_slice(b"foo");
        buf.push(0);
        buf.push(3);
        buf.extend_from_slice(b"bar");
        buf.push(0xC0);
        buf.push(0x00);
        let (name, next) = read_name(&buf, 5).unwrap();
        assert_eq!(name, "bar.foo");
        assert_eq!(next, buf.len());
    }

    #[test]
    fn loopback_responder_round_trips() {
        let responder = spawn_loopback_responder(8765, Ipv4Addr::new(127, 0, 0, 1)).unwrap();
        let client = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        client
            .send_to(
                &encode_query(DEFAULT_SERVICE),
                responder.local_addr().unwrap(),
            )
            .unwrap();
        let mut buf = [0u8; 1024];
        let (len, _) = client.recv_from(&mut buf).unwrap();
        let mut discovery = MdnsDiscovery::default();
        discovery.ingest(buf[..len].to_vec());
        assert_eq!(
            discovery.discover()[0].endpoints,
            vec![Endpoint::Tcp {
                address: "127.0.0.1:8765".into()
            }]
        );
    }

    #[test]
    fn malformed_packets_are_ignored() {
        assert_eq!(parse_records(&[0, 1, 2]), Err(MdnsError::Truncated));
        let mut discovery = MdnsDiscovery::default();
        discovery.ingest(vec![0xff; 3]);
        assert!(discovery.discover().is_empty());
    }

    #[test]
    fn live_query_is_best_effort_and_does_not_hang() {
        let mut discovery = MdnsDiscovery::default();
        let start = std::time::Instant::now();
        let received = discovery.query_live(std::time::Duration::from_millis(50));
        assert!(
            received.is_ok(),
            "a missing multicast route is not an error"
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "the query respects its timeout"
        );
    }
}
