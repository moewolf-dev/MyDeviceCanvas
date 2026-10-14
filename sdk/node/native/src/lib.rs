//! Node native bindings: MemoryLink FakeDevice session (sim path).
use mdc_core::{ConnectionState, DeviceManager, Frame, ManagerEvent, Session, Tile};
use mdc_protocol::InputEvent;
use mdc_simulator::{BoardProfile, FakeDevice, SimError};
use mdc_transport::{TcpTransport, Transport};
use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[napi(object)]
pub struct SurfaceInfo {
    pub id: String,
    pub width: u16,
    pub height: u16,
    #[napi(js_name = "pixelFormat")]
    pub pixel_format: String,
    pub stride: u32,
    pub rotation: u16,
}

#[napi(object)]
pub struct DeviceInfo {
    #[napi(js_name = "deviceId")]
    pub device_id: String,
    pub firmware: String,
    pub surfaces: Vec<SurfaceInfo>,
}

#[napi(object)]
pub struct TileRequest {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    /// Frame id as JS number (protocol uses u64; sim ids stay in u32 range).
    #[napi(js_name = "baseFrameId")]
    pub base_frame_id: u32,
    #[napi(js_name = "surfaceId")]
    pub surface_id: String,
    pub bytes: Buffer,
}

#[napi(object)]
pub struct InputEventDto {
    #[napi(js_name = "deviceId")]
    pub device_id: String,
    #[napi(js_name = "surfaceId")]
    pub surface_id: String,
    #[napi(js_name = "pointerId")]
    pub pointer_id: u16,
    pub phase: String,
    pub x: u16,
    pub y: u16,
}

#[napi(object)]
pub struct MirrorInfoDto {
    #[napi(js_name = "surfaceId")]
    pub surface_id: String,
    #[napi(js_name = "frameId")]
    pub frame_id: u32,
    pub width: u16,
    pub height: u16,
}

#[napi(object)]
pub struct MirrorFrameDto {
    #[napi(js_name = "surfaceId")]
    pub surface_id: String,
    #[napi(js_name = "frameId")]
    pub frame_id: u32,
    pub width: u16,
    pub height: u16,
    pub pixels: Buffer,
}

#[napi(object)]
pub struct MirrorStatsDto {
    #[napi(js_name = "framesReceived")]
    pub frames_received: f64,
    #[napi(js_name = "framesDropped")]
    pub frames_dropped: f64,
    #[napi(js_name = "lastFrameId")]
    pub last_frame_id: f64,
}

#[napi(object)]
pub struct NativeEvent {
    /// "input" | "connected" | "disconnected" | "endpointSwitched" | "error" | "mirror"
    pub kind: String,
    #[napi(js_name = "deviceId")]
    pub device_id: Option<String>,
    pub input: Option<InputEventDto>,
    pub message: Option<String>,
    pub mirror: Option<MirrorInfoDto>,
}

struct SimSession {
    session: Session<Box<dyn Transport>>,
    device: Option<FakeDevice>,
    manager: Arc<Mutex<DeviceManager>>,
}

fn pump_link(sim: &mut SimSession) -> Result<()> {
    let rounds = if sim.device.is_some() { 1 } else { 30 };
    for _ in 0..rounds {
        if let Some(device) = sim.device.as_mut() {
            let device_result = device.poll();
            sim.session
                .poll()
                .map_err(|e| Error::from_reason(e.to_string()))?;
            if let Err(error) = device_result {
                if !matches!(error, SimError::Disconnected) {
                    return Err(Error::from_reason(error.to_string()));
                }
            }
        } else {
            sim.session
                .poll()
                .map_err(|e| Error::from_reason(e.to_string()))?;
        }
        if sim.device.is_none()
            && sim.session.state == ConnectionState::Ready
            && sim.session.pending_request_count() == 0
        {
            break;
        }
        if sim.device.is_none() {
            thread::sleep(Duration::from_millis(5));
        }
    }
    Ok(())
}

#[derive(Default)]
struct State {
    disposed: bool,
    board_id: Option<String>,
    sim: Option<SimSession>,
    last_frame: Option<Vec<u8>>,
}

fn ensure_sim(state: &mut State) -> Result<()> {
    if state.sim.is_some() {
        return Ok(());
    }
    let profile = BoardProfile::from_board_id(state.board_id.as_deref().unwrap_or("default"));
    let (host, mut device) = FakeDevice::pair(profile);
    let manager = Arc::new(Mutex::new(DeviceManager::new()));
    let mut session = Session::new(Box::new(host) as Box<dyn Transport>);
    session.set_manager(manager.clone());
    session
        .connect()
        .map_err(|e| Error::from_reason(e.to_string()))?;
    device
        .poll()
        .map_err(|e| Error::from_reason(e.to_string()))?;
    session
        .poll()
        .map_err(|e| Error::from_reason(e.to_string()))?;
    if session.state != ConnectionState::Ready {
        return Err(Error::from_reason("sim session not ready"));
    }
    if let Some(dev) = session.device.as_ref() {
        let mut mgr = manager
            .lock()
            .map_err(|_| Error::from_reason("manager lock poisoned"))?;
        let _ = mgr.upsert(dev.capabilities.clone());
        mgr.connect_session(&dev.capabilities.device_id, "memory:sim");
    }
    state.sim = Some(SimSession {
        session,
        device: Some(device),
        manager,
    });
    Ok(())
}

fn drain_manager_events(manager: &Arc<Mutex<DeviceManager>>) -> Result<Vec<NativeEvent>> {
    let mut mgr = manager
        .lock()
        .map_err(|_| Error::from_reason("manager lock poisoned"))?;
    let mut out = Vec::new();
    for event in mgr.drain_events() {
        out.push(match event {
            ManagerEvent::Added(id) | ManagerEvent::Connected(id) => NativeEvent {
                kind: "connected".into(),
                device_id: Some(id),
                input: None,
                message: None,
                mirror: None,
            },
            ManagerEvent::Removed(id) | ManagerEvent::Disconnected(id) => NativeEvent {
                kind: "disconnected".into(),
                device_id: Some(id),
                input: None,
                message: None,
                mirror: None,
            },
            ManagerEvent::EndpointSwitched { device_id } => NativeEvent {
                kind: "endpointSwitched".into(),
                device_id: Some(device_id),
                input: None,
                message: None,
                mirror: None,
            },
            ManagerEvent::Input { device_id, event } => NativeEvent {
                kind: "input".into(),
                device_id: Some(device_id.clone()),
                input: Some(InputEventDto {
                    device_id,
                    surface_id: event.surface_id,
                    pointer_id: event.pointer_id,
                    phase: event.phase,
                    x: event.x,
                    y: event.y,
                }),
                message: None,
                mirror: None,
            },
            ManagerEvent::Error { device_id, message } => NativeEvent {
                kind: "error".into(),
                device_id: Some(device_id),
                input: None,
                message: Some(message),
                mirror: None,
            },
            ManagerEvent::Mirror {
                device_id,
                surface_id,
                frame_id,
                width,
                height,
                ..
            } => NativeEvent {
                kind: "mirror".into(),
                device_id: Some(device_id),
                input: None,
                message: None,
                mirror: Some(MirrorInfoDto {
                    surface_id,
                    frame_id: frame_id as u32,
                    width,
                    height,
                }),
            },
        });
    }
    Ok(out)
}

#[napi]
pub struct NativeManager {
    state: Mutex<State>,
}

impl Default for NativeManager {
    fn default() -> Self {
        Self::new()
    }
}

#[napi]
impl NativeManager {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
        }
    }

    /// Connect (or reuse) FakeDevice simulator and return device DTOs.
    #[napi]
    pub fn devices(&self) -> Result<Vec<DeviceInfo>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_ref().unwrap();
        let mgr = sim
            .manager
            .lock()
            .map_err(|_| Error::from_reason("manager lock poisoned"))?;
        let mut out = Vec::new();
        for d in mgr.devices() {
            out.push(DeviceInfo {
                device_id: d.capabilities.device_id.clone(),
                firmware: d.capabilities.firmware.clone(),
                surfaces: d
                    .capabilities
                    .surfaces
                    .iter()
                    .map(|s| SurfaceInfo {
                        id: s.id.clone(),
                        width: s.width,
                        height: s.height,
                        pixel_format: s.pixel_format.clone(),
                        stride: s.stride,
                        rotation: s.rotation,
                    })
                    .collect(),
            });
        }
        Ok(out)
    }

    /// Send RGB565 frame bytes to `surfaceId` via the sim Session.
    #[napi(js_name = "sendFrame")]
    pub fn send_frame(&self, surface_id: String, bytes: Buffer) -> Result<u32> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let copy = bytes.to_vec();
        state.last_frame = Some(copy.clone());
        let sim = state.sim.as_mut().unwrap();
        let (w, h) = sim
            .session
            .device
            .as_ref()
            .and_then(|d| d.capabilities.surfaces.iter().find(|s| s.id == surface_id))
            .map(|s| (s.width, s.height))
            .unwrap_or_else(|| {
                sim.device
                    .as_ref()
                    .map(|device| (device.profile().width, device.profile().height))
                    .unwrap_or((0, 0))
            });
        let expected = usize::from(w) * usize::from(h) * 2;
        if copy.len() != expected {
            return Err(Error::from_reason(format!(
                "frame length {} != expected {}",
                copy.len(),
                expected
            )));
        }
        let req = sim
            .session
            .send_frame(Frame {
                surface_id,
                width: w,
                height: h,
                bytes: copy,
            })
            .map_err(|e| Error::from_reason(e.to_string()))?;
        pump_link(sim)?;
        Ok(req)
    }

    /// Send an RGB565 tile (blocked after reconnect until a full frame ACK).
    #[napi(js_name = "sendTile")]
    pub fn send_tile(&self, tile: TileRequest) -> Result<u32> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let copy = tile.bytes.to_vec();
        let expected = usize::from(tile.width) * usize::from(tile.height) * 2;
        if copy.len() != expected {
            return Err(Error::from_reason(format!(
                "tile length {} != expected {}",
                copy.len(),
                expected
            )));
        }
        let sim = state.sim.as_mut().unwrap();
        let req = sim
            .session
            .send_tile(Tile {
                surface_id: tile.surface_id,
                base_frame_id: u64::from(tile.base_frame_id),
                x: tile.x,
                y: tile.y,
                width: tile.width,
                height: tile.height,
                bytes: copy,
            })
            .map_err(|e| Error::from_reason(e.to_string()))?;
        pump_link(sim)?;
        Ok(req)
    }

    /// Peer-reported current frame id (for tile baseFrameId).
    #[napi(js_name = "currentFrameId")]
    pub fn current_frame_id(&self) -> Result<u32> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        Ok(state
            .sim
            .as_ref()
            .unwrap()
            .device
            .as_ref()
            .map(|device| device.current_frame_id() as u32)
            .unwrap_or(0))
    }

    /// Drain ManagerEvent queue into JS-friendly DTOs.
    #[napi(js_name = "pollEvents")]
    pub fn poll_events(&self) -> Result<Vec<NativeEvent>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        let pumped = pump_link(sim);
        let events = drain_manager_events(&sim.manager)?;
        if events.is_empty() {
            pumped?;
        }
        Ok(events)
    }

    /// Sim-only: emit a touch event from FakeDevice toward the host Session.
    #[napi(js_name = "simulateInput")]
    pub fn simulate_input(
        &self,
        surface_id: String,
        pointer_id: u16,
        phase: String,
        x: u16,
        y: u16,
    ) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        let device = sim
            .device
            .as_mut()
            .ok_or_else(|| Error::from_reason("tcp session cannot inject local touch"))?;
        device
            .emit_input(InputEvent {
                surface_id,
                pointer_id,
                phase,
                x,
                y,
            })
            .map_err(|e| Error::from_reason(e.to_string()))?;
        sim.session
            .poll()
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(())
    }

    /// Form B: ask the device to start/stop mirroring (capability-gated).
    #[napi(js_name = "startMirror")]
    pub fn start_mirror(&self, surface_id: String, mode: String, max_fps: u16) -> Result<u32> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        let request = sim
            .session
            .start_mirror(&surface_id, &mode, max_fps)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        pump_link(sim)?;
        Ok(request)
    }

    #[napi(js_name = "stopMirror")]
    pub fn stop_mirror(&self, surface_id: String) -> Result<u32> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        let request = sim
            .session
            .stop_mirror(&surface_id)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        pump_link(sim)?;
        Ok(request)
    }

    /// Sim-only: push one mirrored frame from FakeDevice toward the host.
    #[napi(js_name = "emitMirrorFrame")]
    pub fn emit_mirror_frame(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        let device = sim
            .device
            .as_mut()
            .ok_or_else(|| Error::from_reason("tcp session cannot emit a local mirror frame"))?;
        device
            .emit_mirror_frame()
            .map_err(|e| Error::from_reason(e.to_string()))?;
        sim.session
            .poll()
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(())
    }

    /// Drain the latest complete mirrored frame (pixels are copied out).
    #[napi(js_name = "takeMirrorFrame")]
    pub fn take_mirror_frame(&self) -> Result<Option<MirrorFrameDto>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        Ok(sim.session.take_mirror_frame().map(|frame| MirrorFrameDto {
            surface_id: frame.surface_id,
            frame_id: frame.frame_id as u32,
            width: frame.width,
            height: frame.height,
            pixels: Buffer::from(frame.bytes),
        }))
    }

    #[napi(js_name = "mirrorStats")]
    pub fn mirror_stats(&self) -> Result<MirrorStatsDto> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        let stats = sim.session.mirror_stats();
        Ok(MirrorStatsDto {
            frames_received: stats.frames_received as f64,
            frames_dropped: stats.frames_dropped as f64,
            last_frame_id: stats.last_frame_id as f64,
        })
    }

    /// N42: read a device configuration value (sim path).
    #[napi(js_name = "getConfig")]
    pub fn get_config(&self, key: String) -> Result<String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        sim.session
            .get_config(&key)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        pump_link(sim)?;
        Ok(sim.session.config(&key).unwrap_or("").to_string())
    }

    /// N42: write a device configuration value and return the accepted value.
    #[napi(js_name = "setConfig")]
    pub fn set_config(&self, key: String, value: String) -> Result<String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        sim.session
            .set_config(&key, &value)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        pump_link(sim)?;
        Ok(sim.session.config(&key).unwrap_or("").to_string())
    }

    /// N55: set a vector scene from a JSON SceneNode (sim path).
    #[napi(js_name = "setScene")]
    pub fn set_scene(&self, json: String) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let node: mdc_protocol::SceneNode = serde_json::from_str(&json)
            .map_err(|e| Error::from_reason(format!("scene json: {e}")))?;
        let sim = state.sim.as_mut().unwrap();
        sim.session
            .set_scene(node)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        pump_link(sim)?;
        Ok(())
    }

    #[napi(js_name = "clearScene")]
    pub fn clear_scene(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        sim.session
            .clear_scene()
            .map_err(|e| Error::from_reason(e.to_string()))?;
        pump_link(sim)?;
        Ok(())
    }

    /// Open a named simulator board before the first `devices()` call.
    #[napi(js_name = "openBoard")]
    pub fn open_board(&self, board_id: String) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        if state.sim.is_some() {
            return Err(Error::from_reason("simulator is already open"));
        }
        state.board_id = Some(board_id);
        ensure_sim(&mut state)
    }

    /// Sim-only: drop the link and emit cancel for pointers that are still down.
    #[napi(js_name = "simulateDisconnect")]
    pub fn simulate_disconnect(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        let device = sim
            .device
            .as_mut()
            .ok_or_else(|| Error::from_reason("tcp session cannot inject a local disconnect"))?;
        device.faults.request_disconnect();
        let device_result = device.poll();
        sim.session
            .poll()
            .map_err(|e| Error::from_reason(e.to_string()))?;
        match device_result {
            Ok(()) | Err(SimError::Disconnected) => Ok(()),
            Err(error) => Err(Error::from_reason(error.to_string())),
        }
    }

    /// Replace the session with a TCP connection to a simulator peer (`host:port`).
    #[napi(js_name = "connectTcp")]
    pub fn connect_tcp(&self, address: String) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        if let Some(mut previous) = state.sim.take() {
            previous.session.disconnect();
        }
        let tcp = TcpTransport::connect(address.trim(), Duration::from_millis(400))
            .map_err(|e| Error::from_reason(e.to_string()))?;
        let manager = Arc::new(Mutex::new(DeviceManager::new()));
        let mut session = Session::new(Box::new(tcp) as Box<dyn Transport>);
        session.set_manager(manager.clone());
        session
            .connect()
            .map_err(|e| Error::from_reason(e.to_string()))?;
        let mut live = SimSession {
            session,
            device: None,
            manager,
        };
        pump_link(&mut live)?;
        if live.session.state != ConnectionState::Ready {
            return Err(Error::from_reason("tcp peer did not finish HELLO"));
        }
        if let Some(device) = live.session.device.as_ref() {
            live.manager
                .lock()
                .map_err(|_| Error::from_reason("manager lock poisoned"))?
                .connect_session(&device.capabilities.device_id, address.trim());
        }
        state.sim = Some(live);
        Ok(())
    }

    #[napi(js_name = "lastFrame")]
    pub fn last_frame(&self) -> Result<Option<Buffer>> {
        let state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        Ok(state.last_frame.clone().map(Buffer::from))
    }

    #[napi]
    pub fn dispose(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if let Some(mut sim) = state.sim.take() {
            sim.session.disconnect();
        }
        state.disposed = true;
        state.last_frame = None;
        Ok(())
    }
}
