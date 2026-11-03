//! TCP FakeDevice peer — unlocks `mdc --address host:port …` against a local simulator.
//! N340: `--ws` serves the same peer over WebSocket for `connectWebSocket`.
//!
//! SIM-R08 fault flags (all optional):
//! - `--drop-next` / `--drop-after N`: explicit E_DROPPED after N packets.
//! - `--lose-after N`: silently drop one packet after N accepted packets
//!   (simulates a lost request/ACK).
//! - `--delay-ticks N`: defer processing of the next packet by N ticks.
//! - `--corrupt-after N` / `--duplicate-after N` / `--split-after N`: corrupt,
//!   duplicate or split one outbound packet after N clean writes.
//! - `--split-all`: split every outbound write in two.
//! - `--swap-next-acks`: buffer two ACKs and emit them in reverse order.
//! - `--exit-after-ms N`: close the process N ms after accept.
//! - `--stall-after-ms N`: keep the socket open but stop servicing it.
//! - `--reboot-after-ms N`: restart the fake device once and emit BootInfo.
//! - `--recaps-after-ms N --recaps-fps F`: send changed Capabilities on a live link.
//! - `--dump-path PATH`: write the accepted framebuffer after every poll (tests).
use mdc_simulator::{BoardProfile, FakeDevice};
use mdc_transport::{TcpTransport, Transport, TransportError, WebSocketTransport};
use std::net::TcpListener;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
enum OutboundFault {
    Corrupt,
    Duplicate,
    Split,
    SplitAll,
    ReorderAcks,
}

struct FaultLink {
    inner: Box<dyn Transport>,
    clean_writes: u32,
    armed: bool,
    done: bool,
    fault: OutboundFault,
    buffered_acks: Vec<Vec<u8>>,
    swaps: u32,
}

impl FaultLink {
    fn new(inner: Box<dyn Transport>, clean_writes: u32, fault: OutboundFault) -> Self {
        Self {
            inner,
            clean_writes,
            armed: clean_writes == 0,
            done: false,
            fault,
            buffered_acks: Vec::new(),
            swaps: 1,
        }
    }
}

impl Transport for FaultLink {
    fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        if matches!(self.fault, OutboundFault::ReorderAcks)
            && self.swaps > 0
            && bytes.len() >= 5
            && bytes[4] == 0x50
        {
            self.buffered_acks.push(bytes.to_vec());
            if self.buffered_acks.len() == 2 {
                let second = self.buffered_acks.pop().unwrap();
                let first = self.buffered_acks.pop().unwrap();
                self.swaps = 0;
                self.inner.write(&second)?;
                return self.inner.write(&first);
            }
            return Ok(());
        }
        if matches!(self.fault, OutboundFault::SplitAll) {
            let middle = bytes.len() / 2;
            self.inner.write(&bytes[..middle])?;
            return self.inner.write(&bytes[middle..]);
        }
        if !self.done && self.armed {
            self.done = true;
            match self.fault {
                OutboundFault::Corrupt => {
                    let mut copy = bytes.to_vec();
                    if copy.len() >= 6 {
                        let last = copy.len() - 1;
                        copy[last] ^= 0xff;
                    }
                    return self.inner.write(&copy);
                }
                OutboundFault::Duplicate => {
                    self.inner.write(bytes)?;
                    return self.inner.write(bytes);
                }
                OutboundFault::Split => {
                    let middle = bytes.len() / 2;
                    self.inner.write(&bytes[..middle])?;
                    return self.inner.write(&bytes[middle..]);
                }
                OutboundFault::SplitAll | OutboundFault::ReorderAcks => {
                    return self.inner.write(bytes);
                }
            }
        }
        if !self.done && self.clean_writes > 0 {
            self.clean_writes -= 1;
            if self.clean_writes == 0 {
                self.armed = true;
            }
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

fn main() {
    let mut bind = "127.0.0.1:9876".to_string();
    let mut board = "esp32-jc3248w535-sim".to_string();
    let mut emit_input = false;
    let mut drop_next = false;
    let mut drop_after = None;
    let mut lose_after = None;
    let mut delay_ticks = 0u64;
    let mut corrupt_after = None;
    let mut duplicate_after = None;
    let mut split_after = None;
    let mut split_all = false;
    let mut swap_acks = false;
    let mut exit_after_ms = None;
    let mut stall_after_ms = None;
    let mut reboot_after_ms = None;
    let mut recaps_after_ms = None;
    let mut recaps_fps = 10u16;
    let mut dump_path: Option<String> = None;
    let mut ws = false;
    let mut positionals = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .unwrap_or_else(|| panic!("{name} needs a value"))
                .parse::<u64>()
                .unwrap_or_else(|_| panic!("{name} needs an unsigned integer"))
        };
        match arg.as_str() {
            "--emit-input" => emit_input = true,
            "--drop-next" => drop_next = true,
            "--ws" => ws = true,
            "--drop-after" => drop_after = Some(value("--drop-after") as u32),
            "--lose-after" => lose_after = Some(value("--lose-after") as u32),
            "--delay-ticks" => delay_ticks = value("--delay-ticks"),
            "--corrupt-after" => corrupt_after = Some(value("--corrupt-after") as u32),
            "--duplicate-after" => duplicate_after = Some(value("--duplicate-after") as u32),
            "--split-after" => split_after = Some(value("--split-after") as u32),
            "--split-all" => split_all = true,
            "--swap-next-acks" => swap_acks = true,
            "--exit-after-ms" => exit_after_ms = Some(value("--exit-after-ms")),
            "--stall-after-ms" => stall_after_ms = Some(value("--stall-after-ms")),
            "--reboot-after-ms" => reboot_after_ms = Some(value("--reboot-after-ms")),
            "--recaps-after-ms" => recaps_after_ms = Some(value("--recaps-after-ms")),
            "--recaps-fps" => recaps_fps = value("--recaps-fps") as u16,
            "--dump-path" => dump_path = Some(args.next().expect("--dump-path needs a value")),
            _ => positionals.push(arg),
        }
    }
    if let Some(value) = positionals.first() {
        bind = value.clone();
    }
    if let Some(value) = positionals.get(1) {
        board = value.clone();
    }
    let profile = BoardProfile::from_board_id(&board);
    let listener = TcpListener::bind(&bind).expect("bind");
    // Print the actual bound address so tests can pass `127.0.0.1:0` instead of
    // reserving a port in a separate process (which races other tests).
    let bound = listener.local_addr().expect("local_addr");
    println!("mdc-sim-peer listening on {bound} board={board}");
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                eprintln!("accept error: {e}");
                continue;
            }
        };
        let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(50)));
        let transport: Box<dyn Transport> = if ws {
            match WebSocketTransport::accept(stream) {
                Ok(t) => Box::new(t),
                Err(e) => {
                    eprintln!("transport: {e}");
                    continue;
                }
            }
        } else {
            match TcpTransport::from_stream(stream) {
                Ok(t) => Box::new(t),
                Err(e) => {
                    eprintln!("transport: {e}");
                    continue;
                }
            }
        };
        let fault = if swap_acks {
            Some((0, OutboundFault::ReorderAcks))
        } else if split_all {
            Some((0, OutboundFault::SplitAll))
        } else if let Some(clean) = corrupt_after {
            Some((clean, OutboundFault::Corrupt))
        } else if let Some(clean) = duplicate_after {
            Some((clean, OutboundFault::Duplicate))
        } else {
            split_after.map(|clean| (clean, OutboundFault::Split))
        };
        let transport = match fault {
            Some((clean, fault)) => {
                Box::new(FaultLink::new(transport, clean, fault)) as Box<dyn Transport>
            }
            None => transport,
        };
        let mut device = FakeDevice::from_transport(transport, profile.clone());
        if emit_input {
            device.emit_down_after_next_frame(6, 7);
        }
        if drop_next {
            device.faults.drop_next();
        }
        if let Some(accept) = drop_after {
            device.faults.drop_one_after(accept);
        }
        if let Some(accept) = lose_after {
            device.faults.drop_silent_after(accept);
        }
        if delay_ticks > 0 {
            device.faults.delay(delay_ticks);
        }
        println!("peer connected; serving FakeDevice {}", profile.device_id);
        let start = Instant::now();
        let mut rebooted = false;
        let mut recapped = false;
        loop {
            if let Some(ms) = stall_after_ms {
                if start.elapsed() >= Duration::from_millis(ms) {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
            }
            if let Some(ms) = exit_after_ms {
                if start.elapsed() >= Duration::from_millis(ms) {
                    println!("peer exiting as requested");
                    return;
                }
            }
            if let Some(ms) = reboot_after_ms {
                if !rebooted && start.elapsed() >= Duration::from_millis(ms) {
                    device.restart();
                    let _ = device.emit_boot_info();
                    rebooted = true;
                    println!("peer rebooted boot_id={}", device.boot_id());
                }
            }
            if let Some(ms) = recaps_after_ms {
                if !recapped && start.elapsed() >= Duration::from_millis(ms) {
                    device.set_advertised_local_fps(recaps_fps);
                    let _ = device.announce_capabilities();
                    recapped = true;
                    println!("peer recapped local_display_fps={recaps_fps}");
                }
            }
            let result = if delay_ticks > 0 {
                device.advance(1)
            } else {
                device.poll()
            };
            match result {
                Ok(()) => {
                    if let Some(path) = &dump_path {
                        let _ = std::fs::write(path, device.pixels());
                    }
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(mdc_simulator::SimError::Disconnected) => {
                    println!("peer disconnected");
                    break;
                }
                Err(e) => {
                    eprintln!("peer error: {e}");
                    break;
                }
            }
        }
    }
}
