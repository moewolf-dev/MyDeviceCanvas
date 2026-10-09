//! Node native bindings: MemoryLink FakeDevice session (sim path).
use mdc_core::{ConnectionState, DeviceManager, Frame, ManagerEvent, Session, Tile};
use mdc_protocol::InputEvent;
use mdc_simulator::{BoardProfile, FakeDevice};
use mdc_transport::MemoryLink;
use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::sync::{Arc, Mutex};

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
pub struct NativeEvent {
    /// "input" | "connected" | "disconnected" | "endpointSwitched" | "error"
    pub kind: String,
    #[napi(js_name = "deviceId")]
    pub device_id: Option<String>,
    pub input: Option<InputEventDto>,
    pub message: Option<String>,
}

struct SimSession {
    session: Session<MemoryLink>,
    device: FakeDevice,
    manager: Arc<Mutex<DeviceManager>>,
}

struct State {
    disposed: bool,
    sim: Option<SimSession>,
    last_frame: Option<Vec<u8>>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            disposed: false,
            sim: None,
            last_frame: None,
        }
    }
}

fn ensure_sim(state: &mut State) -> Result<()> {
    if state.sim.is_some() {
        return Ok(());
    }
    let profile = BoardProfile::default();
    let (host, mut device) = FakeDevice::pair(profile);
    let manager = Arc::new(Mutex::new(DeviceManager::new()));
    let mut session = Session::new(host);
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
        device,
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
            },
            ManagerEvent::Removed(id) | ManagerEvent::Disconnected(id) => NativeEvent {
                kind: "disconnected".into(),
                device_id: Some(id),
                input: None,
                message: None,
            },
            ManagerEvent::EndpointSwitched { device_id } => NativeEvent {
                kind: "endpointSwitched".into(),
                device_id: Some(device_id),
                input: None,
                message: None,
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
            },
            ManagerEvent::Error { device_id, message } => NativeEvent {
                kind: "error".into(),
                device_id: Some(device_id),
                input: None,
                message: Some(message),
            },
        });
    }
    Ok(out)
}

#[napi]
pub struct NativeManager {
    state: Mutex<State>,
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
            .unwrap_or((sim.device.profile().width, sim.device.profile().height));
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
        sim.device
            .poll()
            .map_err(|e| Error::from_reason(e.to_string()))?;
        sim.session
            .poll()
            .map_err(|e| Error::from_reason(e.to_string()))?;
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
        sim.device
            .poll()
            .map_err(|e| Error::from_reason(e.to_string()))?;
        sim.session
            .poll()
            .map_err(|e| Error::from_reason(e.to_string()))?;
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
        Ok(state.sim.as_ref().unwrap().device.current_frame_id() as u32)
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
        sim.device
            .poll()
            .map_err(|e| Error::from_reason(e.to_string()))?;
        sim.session
            .poll()
            .map_err(|e| Error::from_reason(e.to_string()))?;
        drain_manager_events(&sim.manager)
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
        sim.device
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
