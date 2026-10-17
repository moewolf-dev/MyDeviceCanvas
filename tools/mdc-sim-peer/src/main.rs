//! TCP FakeDevice peer — unlocks `mdc --address host:port …` against a local simulator.
//! N340: `--ws` serves the same peer over WebSocket for `connectWebSocket`.
use mdc_simulator::{BoardProfile, FakeDevice};
use mdc_transport::{TcpTransport, WebSocketTransport};
use std::net::TcpListener;
use std::time::Duration;

fn main() {
    let mut bind = "127.0.0.1:9876".to_string();
    let mut board = "esp32-jc3248w535-sim".to_string();
    let mut emit_input = false;
    let mut drop_next = false;
    let mut ws = false;
    let mut positionals = Vec::new();
    for arg in std::env::args().skip(1) {
        if arg == "--emit-input" {
            emit_input = true;
        } else if arg == "--drop-next" {
            drop_next = true;
        } else if arg == "--ws" {
            ws = true;
        } else {
            positionals.push(arg);
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
    println!("mdc-sim-peer listening on {bind} board={board}");
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
        let transport: Box<dyn mdc_transport::Transport> = if ws {
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
        let mut device = FakeDevice::from_transport(transport, profile.clone());
        if emit_input {
            device.emit_down_after_next_frame(6, 7);
        }
        if drop_next {
            device.faults.drop_next();
        }
        println!("peer connected; serving FakeDevice {}", profile.device_id);
        loop {
            match device.poll() {
                Ok(()) => std::thread::sleep(Duration::from_millis(5)),
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
