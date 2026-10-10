//! Commands a Tauri shell should call. Session and Frame/Tile stay in `mdc-core`.
//! Inspector snapshots count events only and do not retain pixel buffers.
use mdc_core::{ConnectionState, CoreError, Frame, Session, Tile};
use mdc_simulator::{BoardProfile, FakeDevice};
use mdc_transport::MemoryLink;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectorSnapshot {
    pub endpoint: String,
    pub device_id: String,
    pub frames_sent: u64,
    pub tiles_sent: u64,
    pub needs_full_frame: bool,
    pub last_error: Option<String>,
}

pub struct DesktopSession {
    session: Session<MemoryLink>,
    device: FakeDevice,
    endpoint: String,
    frames_sent: u64,
    tiles_sent: u64,
    last_error: Option<String>,
}

impl DesktopSession {
    pub fn connect_sim(board_id: &str) -> Result<Self, String> {
        let profile = BoardProfile::from_board_id(board_id);
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().map_err(|e| e.to_string())?;
        device.poll().map_err(|e| e.to_string())?;
        let state = session.poll().map_err(|e| e.to_string())?;
        if state != ConnectionState::Ready {
            return Err(format!("handshake ended in {state:?}"));
        }
        Ok(Self {
            session,
            device,
            endpoint: format!("sim:{board_id}"),
            frames_sent: 0,
            tiles_sent: 0,
            last_error: None,
        })
    }

    pub fn needs_full_frame(&self) -> bool {
        self.session.needs_full_frame
    }

    fn note_err(&mut self, err: CoreError) -> String {
        let text = err.to_string();
        self.last_error = Some(text.clone());
        text
    }

    fn pump(&mut self) -> Result<(), String> {
        self.device.poll().map_err(|e| e.to_string())?;
        self.session.poll().map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn send_color_frame(&mut self) -> Result<u64, String> {
        let width = self.device.profile().width;
        let height = self.device.profile().height;
        let mut bytes = vec![0u8; usize::from(width) * usize::from(height) * 2];
        let colors: [u16; 8] = [
            0xf800, 0x07e0, 0x001f, 0xffe0, 0x07ff, 0xf81f, 0xffff, 0x0000,
        ];
        for y in 0..height {
            for x in 0..width {
                let bar = (usize::from(x) * colors.len()) / usize::from(width).max(1);
                let color = colors[bar.min(colors.len() - 1)];
                let index = (usize::from(y) * usize::from(width) + usize::from(x)) * 2;
                bytes[index] = (color & 0xff) as u8;
                bytes[index + 1] = (color >> 8) as u8;
            }
        }
        match self.session.send_frame(Frame {
            surface_id: "main".into(),
            width,
            height,
            bytes,
        }) {
            Ok(id) => {
                self.pump()?;
                self.frames_sent = self.frames_sent.saturating_add(1);
                self.last_error = None;
                Ok(u64::from(id))
            }
            Err(err) => Err(self.note_err(err)),
        }
    }

    pub fn send_origin_tile(&mut self) -> Result<(), String> {
        if self.session.needs_full_frame {
            return Err(self.note_err(CoreError::NeedFullFrame));
        }
        let base = self.device.current_frame_id();
        match self.session.send_tile(Tile {
            surface_id: "main".into(),
            base_frame_id: base,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            bytes: vec![0xaa, 0xbb],
        }) {
            Ok(_) => {
                self.pump()?;
                self.tiles_sent = self.tiles_sent.saturating_add(1);
                self.last_error = None;
                Ok(())
            }
            Err(err) => Err(self.note_err(err)),
        }
    }

    /// Drop the old Session channel and handshake on a new one. Tiles stay blocked
    /// until a displayed full-frame ACK.
    pub fn switch_sim_endpoint(&mut self) -> Result<(), String> {
        let (host, peer) = MemoryLink::pair();
        self.device.replace_transport(Box::new(peer));
        self.session
            .switch_transport(host)
            .map_err(|e| e.to_string())?;
        self.pump()?;
        if self.session.state != ConnectionState::Ready {
            return Err("switch did not reach Ready".into());
        }
        if !self.session.needs_full_frame {
            return Err("switch must require a full frame before tiles".into());
        }
        self.endpoint = "sim:switched".into();
        Ok(())
    }

    pub fn inspector(&self) -> InspectorSnapshot {
        InspectorSnapshot {
            endpoint: self.endpoint.clone(),
            device_id: self
                .session
                .device
                .as_ref()
                .map(|device| device.capabilities.device_id.clone())
                .unwrap_or_default(),
            frames_sent: self.frames_sent,
            tiles_sent: self.tiles_sent,
            needs_full_frame: self.needs_full_frame(),
            last_error: self.last_error.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_blocks_tiles_until_full_frame_and_inspector_has_no_pixels() {
        let mut app = DesktopSession::connect_sim("default").unwrap();
        app.send_color_frame().unwrap();
        app.send_origin_tile().unwrap();
        app.switch_sim_endpoint().unwrap();
        assert!(app.send_origin_tile().is_err());
        app.send_color_frame().unwrap();
        assert!(!app.needs_full_frame());
        app.send_origin_tile().unwrap();
        let snap = app.inspector();
        assert_eq!(snap.endpoint, "sim:switched");
        assert_eq!(snap.frames_sent, 2);
        assert_eq!(snap.tiles_sent, 2);
        assert!(snap.last_error.is_none());
        let rendered = format!("{snap:?}");
        assert!(!rendered.contains("pixel"));
    }

    #[test]
    fn scene_is_not_a_desktop_command() {
        let (link, _) = MemoryLink::pair();
        let mut session = Session::<MemoryLink>::new(link);
        assert_eq!(session.send_scene(&[]), Err(CoreError::Unsupported));
    }
}
