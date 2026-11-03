//! Device registry and manager events.
//!
//! Merges endpoints by device identity and reports added/removed/connected/
//! endpoint-switch events; it does not own transport or session state.
use super::*;

pub struct Device {
    pub capabilities: Capabilities,
    pub state: ConnectionState,
    pub surfaces: Vec<SurfaceQueue>,
}
#[derive(Debug, Clone, PartialEq)]
pub enum ManagerEvent {
    Added(String),
    Removed(String),
    Connected(String),
    Disconnected(String),
    /// Active endpoint replaced; old Session ACKs must not affect the new one.
    EndpointSwitched {
        device_id: String,
    },
    Input {
        device_id: String,
        event: InputEvent,
    },
    /// N473: a recognised gesture (tap / long-press / drag).
    Gesture {
        device_id: String,
        gesture: Gesture,
    },
    /// Form B (N24): a complete mirrored frame was stored. Metadata only — the
    /// pixels stay in the session inbox (`take_mirror_frame`) so the event queue
    /// stays small.
    Mirror {
        device_id: String,
        surface_id: String,
        frame_id: u64,
        width: u16,
        height: u16,
        frames_received: u64,
        frames_dropped: u64,
    },
    /// N357: a structured telemetry event (N335). Metadata only; the full record
    /// is in the diagnostics queue (`take_diagnostics`).
    Telemetry {
        device_id: String,
        name: String,
        level: Option<String>,
    },
    Error {
        device_id: String,
        message: String,
    },
}
#[derive(Default)]
pub struct DeviceManager {
    devices: BTreeMap<String, Device>,
    events: std::collections::VecDeque<ManagerEvent>,
    /// Single active session endpoint per device id.
    active_endpoints: BTreeMap<String, String>,
}
impl DeviceManager {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn upsert(&mut self, capabilities: Capabilities) -> Result<(), CoreError> {
        if capabilities.device_id.is_empty() || capabilities.surfaces.is_empty() {
            return Err(CoreError::Protocol(
                "device identity/surfaces missing".into(),
            ));
        }
        let id = capabilities.device_id.clone();
        let event = if self.devices.contains_key(&id) {
            ManagerEvent::Connected(id.clone())
        } else {
            ManagerEvent::Added(id.clone())
        };
        let mut device = Device::new(capabilities);
        device.state = ConnectionState::Ready;
        self.devices.insert(id, device);
        self.push_event(event);
        Ok(())
    }
    pub fn remove(&mut self, device_id: &str) -> bool {
        self.active_endpoints.remove(device_id);
        let removed = self.devices.remove(device_id).is_some();
        if removed {
            self.push_event(ManagerEvent::Removed(device_id.into()));
        }
        removed
    }
    pub fn device(&self, device_id: &str) -> Option<&Device> {
        self.devices.get(device_id)
    }
    pub fn device_mut(&mut self, device_id: &str) -> Option<&mut Device> {
        self.devices.get_mut(device_id)
    }
    pub fn devices(&self) -> impl Iterator<Item = &Device> {
        self.devices.values()
    }
    pub fn drain_events(&mut self) -> impl Iterator<Item = ManagerEvent> + '_ {
        self.events.drain(..)
    }
    /// SIM-R09: queued event count for soak/container assertions.
    pub fn event_count(&self) -> usize {
        self.events.len()
    }
    /// Store the single active session endpoint for `device_id` (replaces any prior).
    /// Returns the previous endpoint if this is a switch.
    pub fn connect_session(
        &mut self,
        device_id: &str,
        endpoint: impl Into<String>,
    ) -> Option<String> {
        let endpoint = endpoint.into();
        let previous = self
            .active_endpoints
            .insert(device_id.to_string(), endpoint);
        if let Some(device) = self.devices.get_mut(device_id) {
            device.state = ConnectionState::Ready;
        }
        if previous.is_some() {
            self.push_event(ManagerEvent::EndpointSwitched {
                device_id: device_id.into(),
            });
        } else {
            self.push_event(ManagerEvent::Connected(device_id.into()));
        }
        previous
    }
    pub fn active_endpoint(&self, device_id: &str) -> Option<&str> {
        self.active_endpoints.get(device_id).map(|s| s.as_str())
    }
    pub fn clear_endpoint(&mut self, device_id: &str) {
        if self.active_endpoints.remove(device_id).is_some() {
            self.push_event(ManagerEvent::Disconnected(device_id.into()));
        }
    }
    pub(crate) fn push_event(&mut self, event: ManagerEvent) {
        if self.events.len() >= MAX_MANAGER_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    /// Coalesce consecutive moves. A full queue keeps down/up/cancel, reports an error, and cancels that pointer.
    /// N473: queue a recognised gesture.
    pub fn push_gesture(&mut self, device_id: String, gesture: Gesture) {
        self.events
            .push_back(ManagerEvent::Gesture { device_id, gesture });
        if self.events.len() >= MAX_MANAGER_EVENTS {
            self.events.pop_front();
        }
    }

    pub fn push_input(&mut self, device_id: String, event: InputEvent) {
        if event.phase == "move" {
            if let Some(ManagerEvent::Input {
                device_id: queued_id,
                event: queued,
            }) = self.events.back_mut()
            {
                if queued_id == &device_id
                    && queued.pointer_id == event.pointer_id
                    && queued.phase == "move"
                {
                    queued.x = event.x;
                    queued.y = event.y;
                    return;
                }
            }
        }
        let critical = matches!(event.phase.as_str(), "down" | "up" | "cancel");
        if self.events.len() >= MAX_MANAGER_EVENTS && critical {
            let slots = if event.phase == "cancel" { 2 } else { 3 };
            self.evict_until(MAX_MANAGER_EVENTS - slots);
            self.events.push_back(ManagerEvent::Error {
                device_id: device_id.clone(),
                message: "input queue overflow".into(),
            });
            if event.phase != "cancel" {
                self.events.push_back(ManagerEvent::Input {
                    device_id: device_id.clone(),
                    event: InputEvent {
                        phase: "cancel".into(),
                        ..event.clone()
                    },
                });
            }
            self.events
                .push_back(ManagerEvent::Input { device_id, event });
            return;
        }
        if self.events.len() >= MAX_MANAGER_EVENTS {
            self.evict_until(MAX_MANAGER_EVENTS - 1);
        }
        self.events
            .push_back(ManagerEvent::Input { device_id, event });
    }

    /// N24: record that the session stored a mirrored frame. The bounded event
    /// queue drops the oldest event when full (mirror metadata is not critical).
    #[allow(clippy::too_many_arguments)]
    pub fn push_mirror(
        &mut self,
        device_id: String,
        surface_id: String,
        frame_id: u64,
        width: u16,
        height: u16,
        frames_received: u64,
        frames_dropped: u64,
    ) {
        self.push_event(ManagerEvent::Mirror {
            device_id,
            surface_id,
            frame_id,
            width,
            height,
            frames_received,
            frames_dropped,
        });
    }

    /// N357: record a telemetry event (metadata only) on the bounded queue.
    pub fn push_telemetry(&mut self, device_id: String, name: String, level: Option<String>) {
        self.push_event(ManagerEvent::Telemetry {
            device_id,
            name,
            level,
        });
    }

    fn evict_until(&mut self, limit: usize) {
        while self.events.len() > limit {
            if let Some(index) = self.events.iter().position(
                |event| matches!(event, ManagerEvent::Input { event, .. } if event.phase == "move"),
            ) {
                self.events.remove(index);
            } else {
                self.events.pop_front();
            }
        }
    }
}

impl Device {
    pub fn new(c: Capabilities) -> Self {
        let surfaces = c.surfaces.iter().cloned().map(SurfaceQueue::new).collect();
        Self {
            capabilities: c,
            state: ConnectionState::Disconnected,
            surfaces,
        }
    }
    /// N308: the surface's advertised display profile.
    pub fn surface(&self, surface_id: &str) -> Option<&Surface> {
        self.capabilities
            .surfaces
            .iter()
            .find(|s| s.id == surface_id)
    }
    /// N308: tile cut the host should use for this surface, adapting to the
    /// device-advertised preferred tile size (falls back to the default).
    pub fn plan_surface_tiles(&self, surface_id: &str) -> Vec<(u16, u16, u16, u16)> {
        match self.surface(surface_id) {
            Some(surface) => plan_tiles(surface.width, surface.height, surface.preferred_tile_size),
            None => Vec::new(),
        }
    }
    /// N758: cut every surface into tiles and plan them against one shared byte
    /// budget. Surfaces are equal priority; the caller can reorder the result.
    pub fn plan_shared_tiles(&self, budget: u64) -> mdc_schedule::SharedDecision {
        self.plan_shared_tiles_weighted(budget, &[])
    }

    /// N767: like [`plan_shared_tiles`](Self::plan_shared_tiles) with a relative
    /// weight per surface (missing entries mean 1). The fair pass always serves
    /// each surface once; weights only share the extra budget.
    pub fn plan_shared_tiles_weighted(
        &self,
        budget: u64,
        weights: &[u16],
    ) -> mdc_schedule::SharedDecision {
        let demands: Vec<mdc_schedule::SurfaceDemand> = self
            .capabilities
            .surfaces
            .iter()
            .enumerate()
            .map(|(index, surface)| mdc_schedule::SurfaceDemand {
                surface: index as u32,
                priority: 0,
                weight: weights.get(index).copied().unwrap_or(1),
                tiles: plan_tiles(surface.width, surface.height, surface.preferred_tile_size)
                    .into_iter()
                    .enumerate()
                    .map(|(tile, _)| mdc_schedule::TileCost::new(tile as u32, 1024, false))
                    .collect(),
            })
            .collect();
        mdc_schedule::plan_shared(budget, &demands)
    }

    pub fn validate_frame(&self, f: &Frame) -> Result<(), CoreError> {
        let surface = self
            .capabilities
            .surfaces
            .iter()
            .find(|s| s.id == f.surface_id)
            .ok_or(CoreError::Dimensions)?;
        if f.width != surface.width || f.height != surface.height {
            return Err(CoreError::Dimensions);
        }
        if !surface.pixel_format.is_implemented() {
            return Err(CoreError::Format(surface.pixel_format.to_string()));
        }
        let expected = usize::from(f.width)
            .checked_mul(usize::from(f.height))
            .and_then(|n| n.checked_mul(2))
            .ok_or(CoreError::TooLarge)?;
        if f.bytes.len() != expected || expected > MAX_FRAME_BYTES {
            return Err(CoreError::TooLarge);
        }
        Ok(())
    }
    pub fn send_tile(&self, t: &Tile) -> Result<(), CoreError> {
        let s = self
            .capabilities
            .surfaces
            .iter()
            .find(|s| s.id == t.surface_id)
            .ok_or(CoreError::TileBounds)?;
        let x = t.x as u32 + t.width as u32;
        let y = t.y as u32 + t.height as u32;
        if x > s.width as u32 || y > s.height as u32 {
            return Err(CoreError::TileBounds);
        }
        let expected = (t.width as usize)
            .checked_mul(t.height as usize)
            .and_then(|x| x.checked_mul(2))
            .ok_or(CoreError::TooLarge)?;
        if t.bytes.len() != expected {
            return Err(CoreError::TooLarge);
        }
        Ok(())
    }
    pub fn touch(&self) -> Result<(), CoreError> {
        if self.capabilities.touch {
            Ok(())
        } else {
            Err(CoreError::Unsupported)
        }
    }
    pub fn ota(&self) -> Result<(), CoreError> {
        if self.capabilities.ota {
            Ok(())
        } else {
            Err(CoreError::Unsupported)
        }
    }

    /// Apply runtime-detected surface geometry (FrameOS display_detect idea).
    /// Returns Ok(true) when values changed, Ok(false) when unchanged.
    pub fn update_surface_geometry(
        &mut self,
        surface_id: &str,
        width: u16,
        height: u16,
    ) -> Result<bool, CoreError> {
        if width == 0 || height == 0 {
            return Err(CoreError::Dimensions);
        }
        let _ = mdc_protocol::rgb565_bytes(width, height).map_err(|_| CoreError::TooLarge)?;
        let surface = self
            .capabilities
            .surfaces
            .iter_mut()
            .find(|s| s.id == surface_id)
            .ok_or(CoreError::Dimensions)?;
        if surface.width == width && surface.height == height {
            return Ok(false);
        }
        surface.width = width;
        surface.height = height;
        surface.stride = u32::from(width) * 2;
        if let Some(queue) = self.surfaces.iter_mut().find(|q| q.info.id == surface_id) {
            queue.info.width = width;
            queue.info.height = height;
            queue.info.stride = u32::from(width) * 2;
        }
        Ok(true)
    }
}
