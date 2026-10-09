//! Node native bindings: optional MemoryLink FakeDevice session (sim path).
use mdc_core::{ConnectionState, DeviceManager, Frame, Session};
use mdc_simulator::{BoardProfile, FakeDevice};
use mdc_transport::MemoryLink;
use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::sync::Mutex;

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

struct SimSession {
    session: Session<MemoryLink>,
    device: FakeDevice,
    manager: DeviceManager,
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
    let mut session = Session::new(host);
    let mut manager = DeviceManager::new();
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
        let _ = manager.upsert(dev.capabilities.clone());
        manager.connect_session(&dev.capabilities.device_id, "memory:sim");
    }
    state.sim = Some(SimSession {
        session,
        device,
        manager,
    });
    Ok(())
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
        let mut out = Vec::new();
        for d in sim.manager.devices() {
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
    /// JS facade: `sendFrame(surfaceId, bytes)`.
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
