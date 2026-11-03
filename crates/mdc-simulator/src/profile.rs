//! Board profiles and managed configuration defaults.
use super::*;

/// Board profile dimensions used for Capabilities / pixel buffer.
#[derive(Debug, Clone)]
pub struct BoardProfile {
    pub device_id: String,
    pub width: u16,
    pub height: u16,
    pub touch: bool,
    pub ota: bool,
    /// 彩色刷新承诺。墨水屏必须为 false，不能把彩屏 FPS 写成该板能力。
    pub color_fps: bool,
    /// 触摸坐标旋转，仅接受 0/90/180/270。
    pub rotation: u16,
    /// 第二块 Surface。宽或高为 0 表示没有 overlay。
    pub overlay_width: u16,
    pub overlay_height: u16,
    /// Form B: device renders locally and mirrors frames back to the host.
    pub mirror: bool,
    pub mirror_fps: u16,
    /// N263: advertise `dirty-region` mirroring (a weak device may prefer it).
    pub mirror_dirty_region: bool,
    pub local_display_fps: u16,
    /// N27: local renderer capability advertised on the wire (not inferred from
    /// `mirror`). Drives the model-compiler target backend.
    pub renderer_sprite: bool,
    pub renderer_mesh2d: bool,
    pub renderer_mesh3d: bool,
    pub renderer_skeleton: bool,
    /// N726: advertise dual-quaternion skinning support.
    pub renderer_dqs: bool,
    pub renderer_max_texture: u32,
    pub renderer_max_vertices: u32,
    /// N41: configuration keys this board accepts.
    pub config_keys: Vec<String>,
    /// N596: per-board default configuration values, overlaid on the baseline
    /// managed settings (so a board can ship a different default without the host
    /// hard-coding a board id).
    pub config_defaults: BTreeMap<String, String>,
    /// N308: device-advertised preferred tile edge (0 = no preference; the host
    /// then uses its own default). Reported from the device, not assumed.
    pub preferred_tile_size: u16,
    /// N308: the device accepts partial (tile/dirty) updates.
    pub supports_partial_update: bool,
    /// N308: the device accepts a full framebuffer push (Form A).
    pub supports_full_framebuffer: bool,
}
impl Default for BoardProfile {
    fn default() -> Self {
        Self {
            device_id: "sim-esp32-jc3248w535".into(),
            width: 480,
            height: 320,
            touch: true,
            ota: true,
            color_fps: true,
            rotation: 0,
            overlay_width: 0,
            overlay_height: 0,
            // ESP32-S3 class: Form A only; no GPU to render + mirror at once.
            mirror: false,
            mirror_fps: 0,
            mirror_dirty_region: false,
            local_display_fps: 30,
            // MCU class: sprites only.
            renderer_sprite: true,
            renderer_mesh2d: false,
            renderer_mesh3d: false,
            renderer_skeleton: false,
            renderer_dqs: false,
            renderer_max_texture: 512,
            renderer_max_vertices: 0,
            // N41: a baseline board accepts a few managed settings.
            config_keys: vec![
                "brightness".into(),
                "rotation".into(),
                "wifi_ssid".into(),
                "wifi_password".into(),
                // N71: atlas animation control (only meaningful with a baked atlas).
                "animation".into(),
                // N834: weighted animation blending (`clip:weight,...`).
                "animation_blend".into(),
                // N347: device texture sampling filter.
                "filtering".into(),
                // N586: tolerated unacknowledged tiles before a full frame.
                "tile_missing_threshold".into(),
                // N777: per-surface weights for the host shared budget.
                "surface_weights".into(),
            ],
            // N596: no board-specific overrides by default.
            config_defaults: BTreeMap::new(),
            // N308: an MCU-class panel prefers 32-pixel tiles and takes both
            // partial and full updates (it advertises tile support).
            preferred_tile_size: 32,
            supports_partial_update: true,
            supports_full_framebuffer: true,
        }
    }
}
impl BoardProfile {
    /// Resolve a known board id used by CLI `--board`.
    pub fn from_board_id(id: &str) -> Self {
        match id {
            "esp32-jc3248w535-sim" | "default" | "" => Self::default(),
            "linux-virt" => Self {
                device_id: "sim-linux-virt".into(),
                width: 800,
                height: 480,
                touch: false,
                ota: false,
                color_fps: true,
                // Form B: a Linux board with a GPU renders and mirrors.
                mirror: true,
                mirror_fps: 10,
                mirror_dirty_region: true,
                renderer_mesh2d: true,
                renderer_mesh3d: true,
                renderer_skeleton: true,
                renderer_max_texture: 2048,
                renderer_max_vertices: 200_000,
                // N308: a bigger panel can take bigger tiles.
                preferred_tile_size: 64,
                // N596: a GPU board tolerates a couple of missing tiles before it
                // resynchronises.
                config_defaults: [("tile_missing_threshold".to_string(), "2".to_string())]
                    .into_iter()
                    .collect(),
                ..Self::default()
            },
            "pi4-hdmi" => Self {
                device_id: "sim-pi4-hdmi".into(),
                width: 640,
                height: 480,
                touch: false,
                ota: false,
                color_fps: true,
                mirror: true,
                mirror_fps: 15,
                mirror_dirty_region: true,
                renderer_mesh2d: true,
                renderer_mesh3d: true,
                renderer_skeleton: true,
                renderer_max_texture: 2048,
                renderer_max_vertices: 200_000,
                ..Self::default()
            },
            "waveshare-eink" => Self {
                device_id: "sim-waveshare-eink".into(),
                width: 800,
                height: 480,
                touch: false,
                ota: false,
                color_fps: false,
                ..Self::default()
            },
            "dual-sim" => Self {
                device_id: "sim-dual".into(),
                width: 8,
                height: 4,
                touch: false,
                ota: false,
                color_fps: true,
                overlay_width: 4,
                overlay_height: 2,
                ..Self::default()
            },
            other => Self {
                device_id: format!("sim-{other}"),
                ..Self::default()
            },
        }
    }
}

/// Counters from the last simulate / poll session (inspector JSON).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SimMetrics {
    pub device_id: String,
    pub frames_received: u64,
    pub tiles_received: u64,
    pub inputs_emitted: u64,
    pub ota_commands: u64,
    pub ota_rollbacks: u64,
    pub acks_sent: u64,
    pub errors_sent: u64,
    pub mirror_frames: u64,
    pub mirror_commands: u64,
    pub packages_received: u64,
    pub config_commands: u64,
    pub scene_commands: u64,
    /// N316: diagnostic records (metrics + boot info) emitted to the host.
    pub diagnostics_emitted: u64,
    pub current_frame_id: u64,
    pub pixel_bytes: usize,
}

/// N41: baseline managed configuration.
pub(crate) fn default_config() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    map.insert("brightness".into(), "128".into());
    map.insert("rotation".into(), "0".into());
    map.insert("wifi_ssid".into(), "mdc-sim".into());
    map.insert("wifi_password".into(), "mdc-sim-secret".into());
    // N586: how many unacknowledged tiles the host may accumulate before it must
    // resend a full frame (0 = resynchronise on the first missing tile).
    map.insert(mdc_core::TILES_MISSING_THRESHOLD_CONFIG.into(), "0".into());
    map
}

/// N596: the baseline managed settings overlaid with the board's own defaults.
pub(crate) fn board_config(profile: &BoardProfile) -> BTreeMap<String, String> {
    let mut config = default_config();
    for (key, value) in &profile.config_defaults {
        config.insert(key.clone(), value.clone());
    }
    config
}

/// N41: secret values are never echoed back in a reply.
pub(crate) fn is_secret_key(key: &str) -> bool {
    key.contains("password") || key.contains("token")
}

/// N41: reject obviously invalid values before storing them.
pub(crate) fn validate_config(key: &str, value: &str) -> Result<(), &'static str> {
    if value.len() > 256 {
        return Err("E_VALUE");
    }
    match key {
        "brightness" => value.parse::<u8>().map(|_| ()).map_err(|_| "E_VALUE"),
        "rotation" => match value {
            "0" | "90" | "180" | "270" => Ok(()),
            _ => Err("E_VALUE"),
        },
        "wifi_ssid" => {
            if value.len() <= 32 {
                Ok(())
            } else {
                Err("E_VALUE")
            }
        }
        "wifi_password" => {
            if !value.is_empty() && value.len() <= 63 {
                Ok(())
            } else {
                Err("E_VALUE")
            }
        }
        // N586: a bounded tile-miss threshold.
        "tile_missing_threshold" => value.parse::<u64>().map(|_| ()).map_err(|_| "E_VALUE"),
        // N777: 1..=4 weights, each 1..=16.
        "surface_weights" => mdc_core::parse_surface_weights(value)
            .map(|_| ())
            .ok_or("E_VALUE"),
        _ => Ok(()),
    }
}
