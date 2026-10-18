//! C ABI: opaque MdcSession over FakeDevice or a TCP simulator peer.
use mdc_core::{
    ConnectionState, DeviceManager, DiagnosticRecord, Frame, GestureConfig, ManagerEvent, Session,
    Tile,
};
use mdc_protocol::InputEvent;
use mdc_simulator::{BoardProfile, FakeDevice};
use mdc_transport::{TcpTransport, Transport};
use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::ptr;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub struct MdcSession {
    session: Session<Box<dyn Transport>>,
    device: Option<FakeDevice>,
    manager: Arc<Mutex<DeviceManager>>,
    queued: Vec<ManagerEvent>,
    /// N319: diagnostics drained from the session, oldest first.
    queued_diagnostics: std::collections::VecDeque<DiagnosticRecord>,
}

fn pump(session: &mut MdcSession) -> Result<(), ()> {
    // 设备先写 cancel 再返回断开。会话仍要读走已经进队列的字节。
    let device_result = if let Some(device) = session.device.as_mut() {
        device.poll()
    } else {
        Ok(())
    };
    let session_result = session.session.poll();
    device_result.map_err(|_| ())?;
    session_result.map_err(|_| ())?;
    let drained = session.session.take_diagnostics();
    session.queued_diagnostics.extend(drained);
    Ok(())
}

fn remember_endpoint(session: &MdcSession, endpoint: &str) {
    if let Some(device) = session.session.device.as_ref() {
        if let Ok(mut manager) = session.manager.lock() {
            let _ = manager.connect_session(&device.capabilities.device_id, endpoint);
        }
    }
}

fn open_sim() -> Result<MdcSession, ()> {
    open_board("default")
}

fn open_board(board_id: &str) -> Result<MdcSession, ()> {
    let (host, device) = FakeDevice::pair(BoardProfile::from_board_id(board_id));
    let manager = Arc::new(Mutex::new(DeviceManager::new()));
    let mut session = Session::new(Box::new(host) as Box<dyn Transport>);
    session.set_manager(manager.clone());
    session.connect().map_err(|_| ())?;
    let mut held = MdcSession {
        session,
        device: Some(device),
        manager,
        queued: Vec::new(),
        queued_diagnostics: std::collections::VecDeque::new(),
    };
    pump(&mut held)?;
    if held.session.state != ConnectionState::Ready {
        return Err(());
    }
    remember_endpoint(&held, "memory:sim");
    Ok(held)
}

fn open_tcp(addr: &str) -> Result<MdcSession, ()> {
    let tcp = TcpTransport::connect(addr, Duration::from_millis(400)).map_err(|_| ())?;
    let manager = Arc::new(Mutex::new(DeviceManager::new()));
    let mut session = Session::new(Box::new(tcp) as Box<dyn Transport>);
    session.set_manager(manager.clone());
    session.connect().map_err(|_| ())?;
    let mut held = MdcSession {
        session,
        device: None,
        manager,
        queued: Vec::new(),
        queued_diagnostics: std::collections::VecDeque::new(),
    };
    for _ in 0..40 {
        let _ = held.session.poll();
        if held.session.state == ConnectionState::Ready {
            remember_endpoint(&held, addr);
            return Ok(held);
        }
        thread::sleep(Duration::from_millis(15));
    }
    Err(())
}

/// N366: connect over WebSocket (`ws://host:port`).
fn open_ws(url: &str) -> Result<MdcSession, ()> {
    let ws = mdc_transport::WebSocketTransport::connect(url, Duration::from_millis(400))
        .map_err(|_| ())?;
    let manager = Arc::new(Mutex::new(DeviceManager::new()));
    let mut session = Session::new(Box::new(ws) as Box<dyn Transport>);
    session.set_manager(manager.clone());
    session.connect().map_err(|_| ())?;
    let mut held = MdcSession {
        session,
        device: None,
        manager,
        queued: Vec::new(),
        queued_diagnostics: std::collections::VecDeque::new(),
    };
    for _ in 0..40 {
        let _ = held.session.poll();
        if held.session.state == ConnectionState::Ready {
            remember_endpoint(&held, url);
            return Ok(held);
        }
        thread::sleep(Duration::from_millis(15));
    }
    Err(())
}

fn surface_wh(session: &MdcSession) -> Option<(u16, u16)> {
    if let Some(device) = &session.device {
        return Some((device.profile().width, device.profile().height));
    }
    session
        .session
        .device
        .as_ref()?
        .capabilities
        .surfaces
        .first()
        .map(|surface| (surface.width, surface.height))
}

/// N474: the gesture kind as a stable string.
fn gesture_kind(kind: mdc_core::GestureKind) -> &'static str {
    match kind {
        mdc_core::GestureKind::Tap => "tap",
        mdc_core::GestureKind::LongPress => "long-press",
        mdc_core::GestureKind::Drag => "drag",
    }
}

fn copy_cstr(dst: *mut c_char, cap: usize, text: &str) {
    if dst.is_null() || cap == 0 {
        return;
    }
    let bytes = text.as_bytes();
    let n = bytes.len().min(cap - 1);
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, n);
        *dst.add(n) = 0;
    }
}

/// N506: write the partial-update (tile) counters (null out-params are ignored).
///
/// # Safety
///
/// `session` must be a valid pointer returned by a `mdc_session_*` constructor.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_tile_stats(
    session: *const MdcSession,
    tiles_sent: *mut u64,
    tile_bytes_sent: *mut u64,
) -> c_int {
    let Some(session) = session.as_ref() else {
        return -1;
    };
    let stats = session.session.stats();
    if !tiles_sent.is_null() {
        *tiles_sent = stats.tiles_sent;
    }
    if !tile_bytes_sent.is_null() {
        *tile_bytes_sent = stats.tile_bytes_sent;
    }
    0
}

/// N498: tune gesture recognition (tap window, long-press window, drag distance).
/// Returns 0 on success, negative on error.
///
/// # Safety
///
/// `session` must be a valid pointer returned by a `mdc_session_*` constructor.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_set_gesture_config(
    session: *mut MdcSession,
    tap_max_ms: u64,
    long_press_ms: u64,
    drag_threshold: u16,
) -> c_int {
    let Some(session) = session.as_mut() else {
        return -1;
    };
    session.session.set_gesture_config(GestureConfig {
        tap_max_ms,
        long_press_ms,
        drag_threshold,
    });
    0
}

/// N498: write the current gesture thresholds (null out-params are ignored).
///
/// # Safety
///
/// `session` must be a valid pointer returned by a `mdc_session_*` constructor.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_gesture_config(
    session: *const MdcSession,
    tap_max_ms: *mut u64,
    long_press_ms: *mut u64,
    drag_threshold: *mut u16,
) -> c_int {
    let Some(session) = session.as_ref() else {
        return -1;
    };
    let config = session.session.gesture_config();
    if !tap_max_ms.is_null() {
        *tap_max_ms = config.tap_max_ms;
    }
    if !long_press_ms.is_null() {
        *long_press_ms = config.long_press_ms;
    }
    if !drag_threshold.is_null() {
        *drag_threshold = config.drag_threshold;
    }
    0
}

/// # Safety
/// Returned pointer must be freed with [`mdc_session_destroy`].
#[no_mangle]
pub extern "C" fn mdc_session_create() -> *mut MdcSession {
    match open_sim() {
        Ok(session) => Box::into_raw(Box::new(session)),
        Err(()) => ptr::null_mut(),
    }
}

fn surfaces_of(session: &MdcSession) -> Vec<(String, u16, u16)> {
    session
        .session
        .device
        .as_ref()
        .map(|device| {
            device
                .capabilities
                .surfaces
                .iter()
                .map(|surface| (surface.id.clone(), surface.width, surface.height))
                .collect()
        })
        .unwrap_or_default()
}

/// N307: surfaces with their pixel format, for the SDK surface-format ABI.
fn surface_formats_of(session: &MdcSession) -> Vec<mdc_protocol::PixelFormat> {
    session
        .session
        .device
        .as_ref()
        .map(|device| {
            device
                .capabilities
                .surfaces
                .iter()
                .map(|surface| surface.pixel_format.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// N307: stable code for a `PixelFormat` (see `mdc.h`). Only `0` (RGB565) is
/// implemented; `-1` is an unknown/forward-compatible format.
fn pixel_format_code(format: &mdc_protocol::PixelFormat) -> c_int {
    match format {
        mdc_protocol::PixelFormat::Rgb565 => 0,
        mdc_protocol::PixelFormat::Rgb888 => 1,
        mdc_protocol::PixelFormat::Rgba8888 => 2,
        mdc_protocol::PixelFormat::Gray1 => 3,
        mdc_protocol::PixelFormat::Gray4 => 4,
        mdc_protocol::PixelFormat::Other(_) => -1,
    }
}

/// # Safety
/// `board_id` is a NUL-terminated board name, or null for the default board.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_create_board(board_id: *const c_char) -> *mut MdcSession {
    let name = if board_id.is_null() {
        "default".to_string()
    } else {
        match CStr::from_ptr(board_id).to_str() {
            Ok(text) => text.to_string(),
            Err(_) => return ptr::null_mut(),
        }
    };
    match open_board(&name) {
        Ok(session) => Box::into_raw(Box::new(session)),
        Err(()) => ptr::null_mut(),
    }
}

/// # Safety
/// `session` is null or a live session.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_surface_count(session: *const MdcSession) -> u32 {
    if session.is_null() {
        return 0;
    }
    surfaces_of(&*session).len() as u32
}

/// # Safety
/// `id`, `width`, and `height` must be writable when non-null.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_surface_at(
    session: *const MdcSession,
    index: u32,
    id: *mut c_char,
    id_len: usize,
    width: *mut u16,
    height: *mut u16,
) -> c_int {
    if session.is_null() || width.is_null() || height.is_null() {
        return -1;
    }
    let Some((surface_id, w, h)) = surfaces_of(&*session).into_iter().nth(index as usize) else {
        return -2;
    };
    copy_cstr(id, id_len, &surface_id);
    *width = w;
    *height = h;
    0
}

/// N307: the pixel format code of one surface (see `mdc.h`). Returns -1 when the
/// session is null/not ready or the index is out of range.
///
/// # Safety
/// `session` is null or a live session.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_surface_format_at(
    session: *const MdcSession,
    index: u32,
) -> c_int {
    if session.is_null() {
        return -1;
    }
    match surface_formats_of(&*session)
        .into_iter()
        .nth(index as usize)
    {
        Some(format) => pixel_format_code(&format),
        None => -1,
    }
}

/// N307: non-zero when the host/frame path implements this format code. Only
/// RGB565 (`0`) is implemented in this batch; every other code is rejected.
///
/// # Safety
/// Pure function; no pointers involved.
#[no_mangle]
pub extern "C" fn mdc_pixel_format_is_implemented(code: c_int) -> c_int {
    c_int::from(code == 0)
}

/// N319: emit a metric on the simulator device's diagnostics channel (sim-only).
/// Returns the number of queued diagnostics, or -1 on a null/real session.
///
/// # Safety
/// `session` must be null or a live session; `name`/`unit` are NUL-terminated
/// when non-null.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_emit_metric(
    session: *mut MdcSession,
    name: *const c_char,
    value: f64,
    unit: *const c_char,
) -> c_int {
    if session.is_null() || name.is_null() {
        return -1;
    }
    let held = &mut *session;
    let Ok(name) = CStr::from_ptr(name).to_str() else {
        return -1;
    };
    let unit = if unit.is_null() {
        None
    } else {
        CStr::from_ptr(unit).to_str().ok().map(str::to_string)
    };
    let Some(device) = held.device.as_mut() else {
        return -1;
    };
    if device.emit_metric(name, value, unit.as_deref()).is_err() {
        return -1;
    }
    if pump(held).is_err() {
        return -1;
    }
    held.queued_diagnostics.len() as c_int
}

/// N319: number of diagnostics waiting to be taken.
///
/// # Safety
/// `session` is null or a live session.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_diagnostic_count(session: *const MdcSession) -> u32 {
    if session.is_null() {
        return 0;
    }
    (*session).queued_diagnostics.len() as u32
}

/// N319: pop the oldest diagnostic. Writes `kind` (0 metric, 1 boot info), a
/// name/firmware string, a value (metric value or boot id) and a unit/reset
/// reason. Returns 1 when a record was taken, 0 when the queue is empty, -1 on a
/// bad pointer.
///
/// # Safety
/// All out pointers must be writable when non-null.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_take_diagnostic(
    session: *mut MdcSession,
    kind: *mut c_int,
    name: *mut c_char,
    name_len: usize,
    value: *mut f64,
    unit: *mut c_char,
    unit_len: usize,
) -> c_int {
    if session.is_null() || kind.is_null() || value.is_null() {
        return -1;
    }
    let held = &mut *session;
    let Some(record) = held.queued_diagnostics.pop_front() else {
        return 0;
    };
    match record {
        DiagnosticRecord::Metric(metric) => {
            *kind = 0;
            copy_cstr(name, name_len, &metric.name);
            *value = metric.value;
            copy_cstr(unit, unit_len, metric.unit.as_deref().unwrap_or(""));
        }
        DiagnosticRecord::BootInfo(boot) => {
            *kind = 1;
            copy_cstr(name, name_len, &boot.firmware);
            *value = boot.boot_id as f64;
            copy_cstr(unit, unit_len, boot.reset_reason.as_deref().unwrap_or(""));
        }
        DiagnosticRecord::Telemetry(event) => {
            // N335: kind 2; name = event name, unit = level.
            *kind = 2;
            copy_cstr(name, name_len, &event.name);
            *value = event.event_seq as f64;
            copy_cstr(unit, unit_len, event.level.as_deref().unwrap_or(""));
        }
        DiagnosticRecord::Crash(report) => {
            // N367: kind 3; name = reason, unit = task.
            *kind = 3;
            copy_cstr(name, name_len, &report.reason);
            *value = f64::from(report.pc.unwrap_or(0));
            copy_cstr(unit, unit_len, report.task.as_deref().unwrap_or(""));
        }
    }
    1
}

/// # Safety
/// `session` must be null or a pointer from [`mdc_session_create`].
#[no_mangle]
pub unsafe extern "C" fn mdc_session_destroy(session: *mut MdcSession) {
    if session.is_null() {
        return;
    }
    let mut s = Box::from_raw(session);
    s.session.disconnect();
}

/// # Safety
/// `bytes` must point to `len` readable bytes when non-null.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_send_frame(
    session: *mut MdcSession,
    bytes: *const u8,
    len: usize,
) -> c_int {
    if session.is_null() || bytes.is_null() {
        return -1;
    }
    let s = &mut *session;
    let Some((w, h)) = surface_wh(s) else {
        return -2;
    };
    let expected = usize::from(w) * usize::from(h) * 2;
    if len != expected {
        return -2;
    }
    let slice = std::slice::from_raw_parts(bytes, len);
    match s.session.send_frame(Frame {
        surface_id: "main".into(),
        width: w,
        height: h,
        bytes: slice.to_vec(),
    }) {
        Ok(_) => {
            let _ = pump(s);
            0
        }
        Err(_) => -3,
    }
}

/// # Safety
/// `bytes` must point to `len` readable bytes when non-null.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_send_tile(
    session: *mut MdcSession,
    base_frame_id: u64,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    bytes: *const u8,
    len: usize,
) -> c_int {
    if session.is_null() || bytes.is_null() {
        return -1;
    }
    let s = &mut *session;
    let expected = usize::from(width) * usize::from(height) * 2;
    if len != expected {
        return -2;
    }
    if s.session.needs_full_frame {
        return -4;
    }
    let slice = std::slice::from_raw_parts(bytes, len);
    match s.session.send_tile(Tile {
        surface_id: "main".into(),
        base_frame_id,
        x,
        y,
        width,
        height,
        bytes: slice.to_vec(),
    }) {
        Ok(_) => {
            let _ = pump(s);
            0
        }
        Err(mdc_core::CoreError::NeedFullFrame) => -4,
        Err(_) => -3,
    }
}

/// # Safety
/// `session` must be null or a pointer from [`mdc_session_create`] that has not been destroyed.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_current_frame_id(session: *const MdcSession) -> u64 {
    if session.is_null() {
        return 0;
    }
    (*session)
        .device
        .as_ref()
        .map(|device| device.current_frame_id())
        .unwrap_or(0)
}

/// # Safety
/// `width` / `height` must be writable when non-null.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_surface_size(
    session: *const MdcSession,
    width: *mut u16,
    height: *mut u16,
) -> c_int {
    if session.is_null() || width.is_null() || height.is_null() {
        return -1;
    }
    let s = &*session;
    let Some((w, h)) = surface_wh(s) else {
        return -2;
    };
    *width = w;
    *height = h;
    0
}

/// # Safety
/// `session` must be null or a pointer from [`mdc_session_create`] that has not been destroyed.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_needs_full_frame(session: *const MdcSession) -> c_int {
    if session.is_null() {
        return 0;
    }
    i32::from((*session).session.needs_full_frame)
}

/// # Safety
/// `buf` must be writable for `buflen` bytes when non-null.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_device_id(
    session: *mut MdcSession,
    buf: *mut c_char,
    buflen: usize,
) -> c_int {
    if session.is_null() || buf.is_null() || buflen == 0 {
        return -1;
    }
    let s = &*session;
    let id = s
        .session
        .device
        .as_ref()
        .map(|d| d.capabilities.device_id.as_str())
        .unwrap_or("");
    let bytes = id.as_bytes();
    if bytes.len() + 1 > buflen {
        return -2;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, bytes.len());
    *buf.add(bytes.len()) = 0;
    bytes.len() as c_int
}

/// # Safety
/// `session` must be null or a pointer from [`mdc_session_create`].
/// `bytes` must be readable for `len` when non-null. v1 never encodes Scene.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_send_scene(
    session: *mut MdcSession,
    bytes: *const u8,
    len: usize,
) -> c_int {
    if session.is_null() {
        return -1;
    }
    if bytes.is_null() && len != 0 {
        return -1;
    }
    let payload = if bytes.is_null() {
        &[][..]
    } else {
        std::slice::from_raw_parts(bytes, len)
    };
    match (*session).session.send_scene(payload) {
        Err(mdc_core::CoreError::Unsupported) => -5,
        Ok(_) => 0,
        Err(_) => -3,
    }
}

/// # Safety
/// `addr` must be a NUL-terminated UTF-8 socket address. Returned pointer is freed
/// with [`mdc_session_destroy`].
#[no_mangle]
pub unsafe extern "C" fn mdc_session_connect_tcp(addr: *const c_char) -> *mut MdcSession {
    if addr.is_null() {
        return ptr::null_mut();
    }
    let text = match CStr::from_ptr(addr).to_str() {
        Ok(text) => text,
        Err(_) => return ptr::null_mut(),
    };
    match open_tcp(text) {
        Ok(session) => Box::into_raw(Box::new(session)),
        Err(()) => ptr::null_mut(),
    }
}

/// N366: connect over WebSocket (`ws://host:port`). Returns NULL on failure.
///
/// # Safety
/// `url` is a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_connect_ws(url: *const c_char) -> *mut MdcSession {
    if url.is_null() {
        return ptr::null_mut();
    }
    let text = match CStr::from_ptr(url).to_str() {
        Ok(text) => text,
        Err(_) => return ptr::null_mut(),
    };
    match open_ws(text) {
        Ok(session) => Box::into_raw(Box::new(session)),
        Err(()) => ptr::null_mut(),
    }
}

/// N400: send a pointer event to the device (requires the `touch` capability).
/// Returns 0 on success and a negative value on failure. `phase` is one of
/// `down`/`move`/`up`; a null `surface_id` defaults to `main`.
///
/// # Safety
/// `session` is null or from create/connect; string pointers are NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_send_input(
    session: *mut MdcSession,
    surface_id: *const c_char,
    pointer_id: u16,
    phase: *const c_char,
    x: u16,
    y: u16,
) -> c_int {
    if session.is_null() {
        return -1;
    }
    let surface = if surface_id.is_null() {
        "main".to_string()
    } else {
        CStr::from_ptr(surface_id).to_string_lossy().into_owned()
    };
    let phase = if phase.is_null() {
        "down".to_string()
    } else {
        CStr::from_ptr(phase).to_string_lossy().into_owned()
    };
    let event = InputEvent {
        surface_id: surface,
        pointer_id,
        phase,
        x,
        y,

        node: None,
    };
    match (*session).session.send_input(event) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

/// # Safety
/// Pointers must be writable when non-null. `session` is null or from create/connect.
/// Returns 1 when an event was written, 0 when the queue is empty, negative on error.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_poll_event(
    session: *mut MdcSession,
    kind: *mut c_int,
    device_id: *mut c_char,
    device_id_len: usize,
    phase: *mut c_char,
    phase_len: usize,
    x: *mut u16,
    y: *mut u16,
) -> c_int {
    if session.is_null() || kind.is_null() {
        return -1;
    }
    let session = &mut *session;
    let _ = pump(session);
    if session.queued.is_empty() {
        if let Ok(mut manager) = session.manager.lock() {
            session.queued.extend(manager.drain_events());
        }
    }
    if session.queued.is_empty() {
        return 0;
    }
    let event = session.queued.remove(0);
    let (code, id, phase_text, _pointer, px, py) = match event {
        ManagerEvent::Input { device_id, event } => (
            1,
            device_id,
            event.phase,
            event.pointer_id,
            event.x,
            event.y,
        ),
        // N474: kind 8 carries the gesture kind in `phase`.
        ManagerEvent::Gesture { device_id, gesture } => (
            8,
            device_id,
            gesture_kind(gesture.kind).to_string(),
            gesture.pointer_id,
            gesture.x,
            gesture.y,
        ),
        ManagerEvent::Error { device_id, message } => (2, device_id, message, 0, 0, 0),
        ManagerEvent::Connected(id) | ManagerEvent::Added(id) => (3, id, String::new(), 0, 0, 0),
        ManagerEvent::Disconnected(id) | ManagerEvent::Removed(id) => {
            (4, id, String::new(), 0, 0, 0)
        }
        ManagerEvent::EndpointSwitched { device_id } => (5, device_id, String::new(), 0, 0, 0),
        ManagerEvent::Mirror {
            device_id,
            frame_id,
            ..
        } => (6, device_id, frame_id.to_string(), 0, 0, 0),
        // N357: telemetry name (level is in the diagnostics queue).
        ManagerEvent::Telemetry {
            device_id, name, ..
        } => (7, device_id, name, 0, 0, 0),
    };
    *kind = code;
    copy_cstr(device_id, device_id_len, &id);
    copy_cstr(phase, phase_len, &phase_text);
    if !x.is_null() {
        *x = px;
    }
    if !y.is_null() {
        *y = py;
    }
    1
}

/// N427: like [`mdc_session_poll_event`] but also writes the hit-tested scene node
/// (empty when none) into `node`.
///
/// # Safety
/// All pointers must be writable when non-null; `session` is from create/connect.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_poll_event_with_node(
    session: *mut MdcSession,
    kind: *mut c_int,
    device_id: *mut c_char,
    device_id_len: usize,
    phase: *mut c_char,
    phase_len: usize,
    node: *mut c_char,
    node_len: usize,
    x: *mut u16,
    y: *mut u16,
) -> c_int {
    if session.is_null() || kind.is_null() {
        return -1;
    }
    let session = &mut *session;
    let _ = pump(session);
    if session.queued.is_empty() {
        if let Ok(mut manager) = session.manager.lock() {
            session.queued.extend(manager.drain_events());
        }
    }
    if session.queued.is_empty() {
        return 0;
    }
    let event = session.queued.remove(0);
    let (code, id, phase_text, node_text, px, py) = match event {
        ManagerEvent::Input { device_id, event } => (
            1,
            device_id,
            event.phase,
            event.node.unwrap_or_default(),
            event.x,
            event.y,
        ),
        ManagerEvent::Gesture { device_id, gesture } => (
            8,
            device_id,
            gesture_kind(gesture.kind).to_string(),
            gesture.node.unwrap_or_default(),
            gesture.x,
            gesture.y,
        ),
        ManagerEvent::Error { device_id, message } => (2, device_id, message, String::new(), 0, 0),
        ManagerEvent::Connected(id) | ManagerEvent::Added(id) => {
            (3, id, String::new(), String::new(), 0, 0)
        }
        ManagerEvent::Disconnected(id) | ManagerEvent::Removed(id) => {
            (4, id, String::new(), String::new(), 0, 0)
        }
        ManagerEvent::EndpointSwitched { device_id } => {
            (5, device_id, String::new(), String::new(), 0, 0)
        }
        ManagerEvent::Mirror {
            device_id,
            frame_id,
            ..
        } => (6, device_id, frame_id.to_string(), String::new(), 0, 0),
        ManagerEvent::Telemetry {
            device_id, name, ..
        } => (7, device_id, name, String::new(), 0, 0),
    };
    *kind = code;
    copy_cstr(device_id, device_id_len, &id);
    copy_cstr(phase, phase_len, &phase_text);
    copy_cstr(node, node_len, &node_text);
    if !x.is_null() {
        *x = px;
    }
    if !y.is_null() {
        *y = py;
    }
    1
}

/// N440: write the host-side session counters into the out-params (any of which
/// may be null). Returns 0 on success, negative when `session` is null.
///
/// # Safety
/// Every non-null pointer must be writable.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_stats(
    session: *const MdcSession,
    packets_sent: *mut u64,
    bytes_sent: *mut u64,
    bytes_received: *mut u64,
    acks_received: *mut u64,
    frames_sent: *mut u64,
    dropped_frames: *mut u64,
) -> c_int {
    let Some(session) = session.as_ref() else {
        return -1;
    };
    let stats = session.session.stats();
    if !packets_sent.is_null() {
        *packets_sent = stats.packets_sent;
    }
    if !bytes_sent.is_null() {
        *bytes_sent = stats.bytes_sent;
    }
    if !bytes_received.is_null() {
        *bytes_received = stats.bytes_received;
    }
    if !acks_received.is_null() {
        *acks_received = stats.acks_received;
    }
    if !frames_sent.is_null() {
        *frames_sent = stats.frames_sent;
    }
    if !dropped_frames.is_null() {
        *dropped_frames = stats.dropped_frames;
    }
    0
}

/// # Safety
/// Same as [`mdc_session_poll_event`]. `pointer_id` may be null.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_poll_event_with_pointer(
    session: *mut MdcSession,
    kind: *mut c_int,
    device_id: *mut c_char,
    device_id_len: usize,
    phase: *mut c_char,
    phase_len: usize,
    pointer_id: *mut u16,
    x: *mut u16,
    y: *mut u16,
) -> c_int {
    if session.is_null() || kind.is_null() {
        return -1;
    }
    let held = &mut *session;
    let _ = pump(held);
    if held.queued.is_empty() {
        if let Ok(mut manager) = held.manager.lock() {
            held.queued.extend(manager.drain_events());
        }
    }
    if held.queued.is_empty() {
        return 0;
    }
    let event = held.queued.remove(0);
    let (code, id, phase_text, pointer, px, py) = match event {
        ManagerEvent::Input { device_id, event } => (
            1,
            device_id,
            event.phase,
            event.pointer_id,
            event.x,
            event.y,
        ),
        // N474: kind 8 carries the gesture kind in `phase`.
        ManagerEvent::Gesture { device_id, gesture } => (
            8,
            device_id,
            gesture_kind(gesture.kind).to_string(),
            gesture.pointer_id,
            gesture.x,
            gesture.y,
        ),
        ManagerEvent::Error { device_id, message } => (2, device_id, message, 0, 0, 0),
        ManagerEvent::Connected(id) | ManagerEvent::Added(id) => (3, id, String::new(), 0, 0, 0),
        ManagerEvent::Disconnected(id) | ManagerEvent::Removed(id) => {
            (4, id, String::new(), 0, 0, 0)
        }
        ManagerEvent::EndpointSwitched { device_id } => (5, device_id, String::new(), 0, 0, 0),
        ManagerEvent::Mirror {
            device_id,
            frame_id,
            ..
        } => (6, device_id, frame_id.to_string(), 0, 0, 0),
        // N357: telemetry name (level is in the diagnostics queue).
        ManagerEvent::Telemetry {
            device_id, name, ..
        } => (7, device_id, name, 0, 0, 0),
    };
    *kind = code;
    copy_cstr(device_id, device_id_len, &id);
    copy_cstr(phase, phase_len, &phase_text);
    if !pointer_id.is_null() {
        *pointer_id = pointer;
    }
    if !x.is_null() {
        *x = px;
    }
    if !y.is_null() {
        *y = py;
    }
    1
}

/// # Safety
/// `session` must be a simulator session. `phase` is a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn mdc_session_simulate_input(
    session: *mut MdcSession,
    phase: *const c_char,
    x: u16,
    y: u16,
) -> c_int {
    if session.is_null() || phase.is_null() {
        return -1;
    }
    let phase = match CStr::from_ptr(phase).to_str() {
        Ok(phase) => phase,
        Err(_) => return -1,
    };
    let session = &mut *session;
    let Some(device) = session.device.as_mut() else {
        return -6;
    };
    if device
        .emit_input(InputEvent {
            surface_id: "main".into(),
            pointer_id: 1,
            phase: phase.into(),
            x,
            y,

            node: None,
        })
        .is_err()
    {
        return -3;
    }
    if session.session.poll().is_err() {
        return -3;
    }
    0
}

/// # Safety
/// Currently a no-op placeholder for ABI symmetry; only NULL is valid until helpers allocate.
#[no_mangle]
pub unsafe extern "C" fn mdc_free(ptr: *mut std::ffi::c_void) {
    let _ = ptr;
}

unsafe fn mirror_surface(surface_id: *const c_char) -> String {
    if surface_id.is_null() {
        "main".to_string()
    } else {
        CStr::from_ptr(surface_id).to_string_lossy().into_owned()
    }
}

/// # Safety
/// `session` must come from a create/connect function and be alive.
#[no_mangle]
pub unsafe extern "C" fn mdc_start_mirror(
    session: *mut MdcSession,
    surface_id: *const c_char,
    max_fps: u16,
) -> c_int {
    let Some(session) = session.as_mut() else {
        return -1;
    };
    let surface = mirror_surface(surface_id);
    match session
        .session
        .start_mirror(&surface, "full-frame", max_fps)
    {
        Ok(_) => {
            let _ = pump(session);
            0
        }
        Err(_) => -1,
    }
}

/// # Safety
/// `session` must come from a create/connect function and be alive.
#[no_mangle]
pub unsafe extern "C" fn mdc_stop_mirror(
    session: *mut MdcSession,
    surface_id: *const c_char,
) -> c_int {
    let Some(session) = session.as_mut() else {
        return -1;
    };
    let surface = mirror_surface(surface_id);
    match session.session.stop_mirror(&surface) {
        Ok(_) => {
            let _ = pump(session);
            0
        }
        Err(_) => -1,
    }
}

/// # Safety
/// `session` must be a simulator session.
#[no_mangle]
pub unsafe extern "C" fn mdc_emit_mirror_frame(session: *mut MdcSession) -> c_int {
    let Some(session) = session.as_mut() else {
        return -1;
    };
    let Some(device) = session.device.as_mut() else {
        return -1;
    };
    match device.emit_mirror_frame() {
        Ok(()) => {
            let _ = session.session.poll();
            0
        }
        Err(_) => -1,
    }
}

/// # Safety
/// `session` must be alive; `out` must point to `cap` writable bytes when non-NULL.
#[no_mangle]
pub unsafe extern "C" fn mdc_take_mirror_frame(
    session: *mut MdcSession,
    width: *mut u16,
    height: *mut u16,
    frame_id: *mut u64,
    out: *mut u8,
    cap: usize,
) -> i64 {
    let Some(session) = session.as_mut() else {
        return -1;
    };
    let Some(frame) = session.session.take_mirror_frame() else {
        return -1;
    };
    if !width.is_null() {
        *width = frame.width;
    }
    if !height.is_null() {
        *height = frame.height;
    }
    if !frame_id.is_null() {
        *frame_id = frame.frame_id;
    }
    let len = frame.bytes.len();
    if out.is_null() {
        return len as i64;
    }
    if cap < len {
        return -2;
    }
    ptr::copy_nonoverlapping(frame.bytes.as_ptr(), out, len);
    len as i64
}

/// # Safety
/// `session` must be alive; the out pointers may be NULL.
#[no_mangle]
pub unsafe extern "C" fn mdc_mirror_stats(
    session: *const MdcSession,
    received: *mut u64,
    dropped: *mut u64,
) -> c_int {
    let Some(session) = session.as_ref() else {
        return -1;
    };
    let stats = session.session.mirror_stats();
    if !received.is_null() {
        *received = stats.frames_received;
    }
    if !dropped.is_null() {
        *dropped = stats.frames_dropped;
    }
    0
}

/// # Safety
/// `session` must be alive; `json` must be a NUL-terminated UTF-8 SceneNode.
/// N63: set a vector scene on a simulator session. 0 or negative.
#[no_mangle]
pub unsafe extern "C" fn mdc_set_scene(session: *mut MdcSession, json: *const c_char) -> c_int {
    let Some(session) = session.as_mut() else {
        return -1;
    };
    if json.is_null() {
        return -1;
    }
    let text = CStr::from_ptr(json).to_string_lossy();
    let node: mdc_protocol::SceneNode = match serde_json::from_str(&text) {
        Ok(node) => node,
        Err(_) => return -2,
    };
    match session.session.set_scene(node) {
        Ok(_) => {
            let _ = pump(session);
            0
        }
        Err(_) => -1,
    }
}

/// # Safety
/// `session` must be alive. N63: clear the vector scene. 0 or negative.
#[no_mangle]
pub unsafe extern "C" fn mdc_clear_scene(session: *mut MdcSession) -> c_int {
    let Some(session) = session.as_mut() else {
        return -1;
    };
    match session.session.clear_scene() {
        Ok(_) => {
            let _ = pump(session);
            0
        }
        Err(_) => -1,
    }
}

/// # Safety
/// `session` must be alive. `bytes` must be readable for `len` when non-null.
/// N243: upload a runtime package (`.mdcpkg` bytes). 0 on send, negative on
/// error (-1 session, -2 bad bytes). Check `mdc_package_ready` for acceptance.
#[no_mangle]
pub unsafe extern "C" fn mdc_install_package(
    session: *mut MdcSession,
    bytes: *const u8,
    len: usize,
) -> c_int {
    let Some(session) = session.as_mut() else {
        return -1;
    };
    if bytes.is_null() || len == 0 {
        return -2;
    }
    let data = std::slice::from_raw_parts(bytes, len);
    match session.session.install_package(data) {
        Ok(_) => {
            let _ = pump(session);
            0
        }
        Err(_) => -3,
    }
}

/// # Safety
/// `session` must be alive. N243: 1 when the last accepted package is
/// render-ready, 0 otherwise, -1 on a bad session.
#[no_mangle]
pub unsafe extern "C" fn mdc_package_ready(session: *mut MdcSession) -> c_int {
    let Some(session) = session.as_mut() else {
        return -1;
    };
    let _ = pump(session);
    match session.device.as_ref() {
        Some(device) if device.package_ready() => 1,
        _ => 0,
    }
}

/// # Safety
/// `session` alive; `key` NUL-terminated; `out` points to `out_len` writable
/// bytes when non-null. N244: read a config value into `out`; returns the length
/// or a negative error (-1 session, -2 bad args, -3 unknown key, -4 too small).
#[no_mangle]
pub unsafe extern "C" fn mdc_get_config(
    session: *mut MdcSession,
    key: *const c_char,
    out: *mut c_char,
    out_len: usize,
) -> c_int {
    let Some(session) = session.as_mut() else {
        return -1;
    };
    if key.is_null() || out.is_null() || out_len == 0 {
        return -2;
    }
    let key = CStr::from_ptr(key).to_string_lossy().to_string();
    if session.session.get_config(&key).is_err() {
        return -3;
    }
    let _ = pump(session);
    let value = session.session.config(&key).unwrap_or("");
    let bytes = value.as_bytes();
    if bytes.len() + 1 > out_len {
        return -4;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), out as *mut u8, bytes.len());
    *out.add(bytes.len()) = 0;
    bytes.len() as c_int
}

/// # Safety
/// `session` alive; `key`/`value` NUL-terminated. N244: write a config value.
/// 0 on success, negative on error (-1 session, -2 bad args, -3 device rejected).
#[no_mangle]
pub unsafe extern "C" fn mdc_set_config(
    session: *mut MdcSession,
    key: *const c_char,
    value: *const c_char,
) -> c_int {
    let Some(session) = session.as_mut() else {
        return -1;
    };
    if key.is_null() || value.is_null() {
        return -2;
    }
    let key = CStr::from_ptr(key).to_string_lossy().to_string();
    let value = CStr::from_ptr(value).to_string_lossy().to_string();
    match session.session.set_config(&key, &value) {
        Ok(_) => {
            let _ = pump(session);
            0
        }
        Err(_) => -3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_abi_connect_ws_rejects_an_unreachable_url() {
        let url = std::ffi::CString::new("ws://127.0.0.1:1").unwrap();
        let session = unsafe { mdc_session_connect_ws(url.as_ptr()) };
        assert!(session.is_null());
    }

    #[test]
    fn c_abi_session_stats_reports_counters() {
        // N440: the counters are readable from C.
        let session = mdc_session_create();
        assert!(!session.is_null());
        unsafe {
            let (mut sent, mut bytes, mut received, mut acks, mut frames, mut dropped) =
                (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
            assert_eq!(
                mdc_session_stats(
                    session,
                    &mut sent,
                    &mut bytes,
                    &mut received,
                    &mut acks,
                    &mut frames,
                    &mut dropped,
                ),
                0
            );
            // A fresh session has sent its HELLO, so the counters are non-zero.
            assert!(sent >= 1, "packets sent: {sent}");
            assert!(bytes > 0);
            // Null out-params are ignored.
            assert_eq!(
                mdc_session_stats(
                    session,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ),
                0
            );
            mdc_session_destroy(session);
        }
    }

    #[test]
    fn c_abi_poll_event_with_node_writes_the_hit_node() {
        // N427: the hit-tested scene node is available to C callers.
        let session = mdc_session_create();
        assert!(!session.is_null());
        unsafe {
            // A scene rect covering the whole surface, then a touch inside it.
            let scene = std::ffi::CString::new(
                r##"{"name":"root","kind":"rect","rect":[0,0,480,320],"value":"#fff"}"##,
            )
            .unwrap();
            assert_eq!(mdc_set_scene(session, scene.as_ptr()), 0);
            let phase = std::ffi::CString::new("down").unwrap();
            // The device emits the input (so it hit-tests the scene).
            assert!(mdc_session_simulate_input(session, phase.as_ptr(), 10, 10) >= 0);
            // Drain the events until the input arrives.
            let mut kind = -1;
            let mut id = [0i8; 64];
            let mut ph = [0i8; 16];
            let mut node = [0i8; 64];
            let (mut x, mut y) = (0u16, 0u16);
            let mut found = None;
            for _ in 0..8 {
                let rc = mdc_session_poll_event_with_node(
                    session,
                    &mut kind,
                    id.as_mut_ptr(),
                    id.len(),
                    ph.as_mut_ptr(),
                    ph.len(),
                    node.as_mut_ptr(),
                    node.len(),
                    &mut x,
                    &mut y,
                );
                if rc <= 0 {
                    break;
                }
                if kind == 1 {
                    found = Some(
                        std::ffi::CStr::from_ptr(node.as_ptr())
                            .to_string_lossy()
                            .into_owned(),
                    );
                    break;
                }
            }
            assert_eq!(found.as_deref(), Some("root"), "the hit node is reported");
            mdc_session_destroy(session);
        }
    }

    #[test]
    fn c_abi_send_input_reaches_the_device() {
        // N400: the host can send a pointer event when the device enables touch.
        let session = mdc_session_create();
        assert!(!session.is_null());
        unsafe {
            let surface = std::ffi::CString::new("main").unwrap();
            let phase = std::ffi::CString::new("down").unwrap();
            assert_eq!(
                mdc_session_send_input(session, surface.as_ptr(), 2, phase.as_ptr(), 5, 6),
                0
            );
            mdc_session_destroy(session);
        }
    }

    #[test]
    fn c_abi_diagnostics_round_trip() {
        let session = mdc_session_create();
        assert!(!session.is_null());
        unsafe {
            let name = std::ffi::CString::new("fps").unwrap();
            let unit = std::ffi::CString::new("fps").unwrap();
            assert!(mdc_session_emit_metric(session, name.as_ptr(), 29.5, unit.as_ptr()) >= 1);
            assert_eq!(mdc_session_diagnostic_count(session), 1);
            let mut kind = -1;
            let mut out_name = [0i8; 32];
            let mut value = 0.0f64;
            let mut out_unit = [0i8; 16];
            assert_eq!(
                mdc_session_take_diagnostic(
                    session,
                    &mut kind,
                    out_name.as_mut_ptr(),
                    out_name.len(),
                    &mut value,
                    out_unit.as_mut_ptr(),
                    out_unit.len(),
                ),
                1
            );
            assert_eq!(kind, 0);
            assert_eq!(
                std::ffi::CStr::from_ptr(out_name.as_ptr())
                    .to_str()
                    .unwrap(),
                "fps"
            );
            assert_eq!(value, 29.5);
            assert_eq!(
                std::ffi::CStr::from_ptr(out_unit.as_ptr())
                    .to_str()
                    .unwrap(),
                "fps"
            );
            assert_eq!(mdc_session_diagnostic_count(session), 0);
            // An empty queue returns 0, not an error.
            assert_eq!(
                mdc_session_take_diagnostic(
                    session,
                    &mut kind,
                    out_name.as_mut_ptr(),
                    out_name.len(),
                    &mut value,
                    out_unit.as_mut_ptr(),
                    out_unit.len(),
                ),
                0
            );
            mdc_session_destroy(session);
        }
    }

    #[test]
    fn c_abi_frame_copy_returns_to_zero_after_destroy() {
        let session = mdc_session_create();
        assert!(!session.is_null());
        unsafe {
            let mut w = 0u16;
            let mut h = 0u16;
            assert_eq!(mdc_session_surface_size(session, &mut w, &mut h), 0);
            let len = usize::from(w) * usize::from(h) * 2;
            let frame = vec![0x11u8; len];
            assert_eq!(
                mdc_session_send_frame(session, frame.as_ptr(), frame.len()),
                0
            );
            assert_eq!((*session).session.copy_outstanding(), 0);
            assert_eq!((*session).session.copy_peak(), len * 3);
            (*session).session.disconnect();
            assert_eq!((*session).session.copy_outstanding(), 0);
            assert_eq!(
                mdc_session_send_frame(session, frame.as_ptr(), frame.len()),
                -3
            );
            assert_eq!((*session).session.copy_outstanding(), 0);
            mdc_session_destroy(session);
        }
    }

    #[test]
    fn c_abi_frame_then_tile_round_trip() {
        let session = mdc_session_create();
        assert!(!session.is_null());
        unsafe {
            let mut w = 0u16;
            let mut h = 0u16;
            assert_eq!(mdc_session_surface_size(session, &mut w, &mut h), 0);
            assert_eq!((w, h), (480, 320));
            let len = usize::from(w) * usize::from(h) * 2;
            let frame = vec![0x11u8; len];
            assert_eq!(
                mdc_session_send_frame(session, frame.as_ptr(), frame.len()),
                0
            );
            let fid = mdc_session_current_frame_id(session);
            assert!(fid > 0);
            assert_eq!(mdc_session_needs_full_frame(session), 0);
            let tile = [0xAAu8, 0xBB];
            assert_eq!(
                mdc_session_send_tile(session, fid, 0, 0, 1, 1, tile.as_ptr(), tile.len()),
                0
            );
            let mut buf = [0i8; 64];
            let n = mdc_session_device_id(session, buf.as_mut_ptr(), buf.len());
            assert!(n > 0);
            assert_eq!(mdc_session_send_scene(session, std::ptr::null(), 0), -5);
            mdc_session_destroy(std::ptr::null_mut());
            let mut kind = 0;
            let mut device_id = [0i8; 64];
            let mut phase = [0i8; 16];
            let mut x = 0u16;
            let mut y = 0u16;
            let phase_c = std::ffi::CString::new("down").unwrap();
            assert_eq!(
                mdc_session_simulate_input(session, phase_c.as_ptr(), 4, 5),
                0
            );
            let mut saw_input = false;
            for _ in 0..8 {
                let rc = mdc_session_poll_event(
                    session,
                    &mut kind,
                    device_id.as_mut_ptr(),
                    device_id.len(),
                    phase.as_mut_ptr(),
                    phase.len(),
                    &mut x,
                    &mut y,
                );
                if rc == 0 {
                    break;
                }
                if kind == 1 {
                    saw_input = true;
                    break;
                }
            }
            assert!(saw_input);
            assert_eq!(x, 4);
            assert_eq!(y, 5);
            mdc_session_destroy(session);
        }
    }

    #[test]
    fn tcp_session_reaches_ready_against_fake_device() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (sock, _) = listener.accept().unwrap();
            let _ = sock.set_read_timeout(Some(Duration::from_millis(50)));
            let transport = TcpTransport::from_stream(sock).unwrap();
            let mut device =
                FakeDevice::from_transport(Box::new(transport), BoardProfile::default());
            for _ in 0..80 {
                if device.poll().is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
        });
        let text = std::ffi::CString::new(addr.to_string()).unwrap();
        let session = unsafe { mdc_session_connect_tcp(text.as_ptr()) };
        assert!(!session.is_null());
        unsafe {
            let mut id = [0i8; 64];
            assert!(mdc_session_device_id(session, id.as_mut_ptr(), id.len()) > 0);
            mdc_session_destroy(session);
        }
        let _ = server.join();
    }

    #[test]
    fn tcp_scripted_input_reaches_poll_event() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (sock, _) = listener.accept().unwrap();
            let _ = sock.set_read_timeout(Some(Duration::from_millis(50)));
            let transport = TcpTransport::from_stream(sock).unwrap();
            let mut device =
                FakeDevice::from_transport(Box::new(transport), BoardProfile::default());
            device.emit_input_after_next_frame(InputEvent {
                surface_id: "main".into(),
                pointer_id: 1,
                phase: "down".into(),
                x: 6,
                y: 7,

                node: None,
            });
            for _ in 0..120 {
                if device.poll().is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
        });
        let text = std::ffi::CString::new(addr.to_string()).unwrap();
        let session = unsafe { mdc_session_connect_tcp(text.as_ptr()) };
        assert!(!session.is_null());
        unsafe {
            let mut width = 0u16;
            let mut height = 0u16;
            assert_eq!(
                mdc_session_surface_size(session, &mut width, &mut height),
                0
            );
            let frame = vec![0u8; usize::from(width) * usize::from(height) * 2];
            assert_eq!(
                mdc_session_send_frame(session, frame.as_ptr(), frame.len()),
                0
            );
            let mut saw = false;
            for _ in 0..40 {
                let mut kind = 0;
                let mut device_id = [0i8; 64];
                let mut phase = [0i8; 16];
                let mut x = 0u16;
                let mut y = 0u16;
                let rc = mdc_session_poll_event(
                    session,
                    &mut kind,
                    device_id.as_mut_ptr(),
                    device_id.len(),
                    phase.as_mut_ptr(),
                    phase.len(),
                    &mut x,
                    &mut y,
                );
                if rc == 1 && kind == 1 && x == 6 && y == 7 {
                    saw = true;
                    break;
                }
                thread::sleep(Duration::from_millis(15));
            }
            assert!(saw);
            mdc_session_destroy(session);
        }
        let _ = server.join();
    }

    #[test]
    fn c_abi_lists_dual_surfaces_and_keeps_main_size() {
        let board = std::ffi::CString::new("dual-sim").unwrap();
        let session = unsafe { mdc_session_create_board(board.as_ptr()) };
        assert!(!session.is_null());
        unsafe {
            assert_eq!(mdc_session_surface_count(session), 2);
            let mut id = [0i8; 32];
            let mut width = 0u16;
            let mut height = 0u16;
            assert_eq!(
                mdc_session_surface_at(
                    session,
                    0,
                    id.as_mut_ptr(),
                    id.len(),
                    &mut width,
                    &mut height
                ),
                0
            );
            assert_eq!((width, height), (8, 4));
            assert_eq!(
                mdc_session_surface_at(
                    session,
                    1,
                    id.as_mut_ptr(),
                    id.len(),
                    &mut width,
                    &mut height
                ),
                0
            );
            assert_eq!((width, height), (4, 2));
            let mut main_w = 0u16;
            let mut main_h = 0u16;
            assert_eq!(
                mdc_session_surface_size(session, &mut main_w, &mut main_h),
                0
            );
            assert_eq!((main_w, main_h), (8, 4));
            let created = mdc_session_create();
            assert_eq!(mdc_session_surface_count(created), 1);
            // N307: the surface format enum is exposed; only RGB565 is implemented.
            assert_eq!(mdc_session_surface_format_at(session, 0), 0);
            assert_eq!(mdc_session_surface_format_at(session, 1), 0);
            assert_eq!(mdc_session_surface_format_at(session, 9), -1);
            assert_eq!(mdc_pixel_format_is_implemented(0), 1);
            assert_eq!(mdc_pixel_format_is_implemented(2), 0);
            mdc_session_destroy(created);
            mdc_session_destroy(session);
        }
    }

    #[test]
    fn poll_event_reports_disconnect_cancel() {
        let session = mdc_session_create();
        assert!(!session.is_null());
        let held = unsafe { &mut *session };
        held.device
            .as_mut()
            .unwrap()
            .emit_input(InputEvent {
                surface_id: "main".into(),
                pointer_id: 3,
                phase: "down".into(),
                x: 6,
                y: 7,

                node: None,
            })
            .unwrap();
        held.device.as_mut().unwrap().faults.request_disconnect();
        unsafe {
            let mut kind = 0;
            let mut device_id = [0i8; 64];
            let mut phase = [0i8; 16];
            let mut pointer = 0u16;
            let mut x = 0u16;
            let mut y = 0u16;
            let mut saw_cancel = false;
            for _ in 0..8 {
                let rc = mdc_session_poll_event_with_pointer(
                    session,
                    &mut kind,
                    device_id.as_mut_ptr(),
                    device_id.len(),
                    phase.as_mut_ptr(),
                    phase.len(),
                    &mut pointer,
                    &mut x,
                    &mut y,
                );
                if rc != 1 {
                    continue;
                }
                let text = std::ffi::CStr::from_ptr(phase.as_ptr()).to_string_lossy();
                if kind == 1 && text == "cancel" {
                    assert_eq!(pointer, 3);
                    assert_eq!((x, y), (6, 7));
                    saw_cancel = true;
                    break;
                }
            }
            assert!(saw_cancel);
            mdc_session_destroy(session);
        }
    }

    #[test]
    fn mirror_start_emit_and_take() {
        unsafe {
            let board = std::ffi::CString::new("linux-virt").unwrap();
            let session = mdc_session_create_board(board.as_ptr());
            assert!(!session.is_null());
            assert_eq!(mdc_start_mirror(session, std::ptr::null(), 15), 0);
            assert_eq!(mdc_emit_mirror_frame(session), 0);

            let mut width = 0u16;
            let mut height = 0u16;
            let mut frame_id = 0u64;
            let mut buf = vec![0u8; 800 * 480 * 2];
            let n = mdc_take_mirror_frame(
                session,
                &mut width,
                &mut height,
                &mut frame_id,
                buf.as_mut_ptr(),
                buf.len(),
            );
            assert_eq!(n, (800 * 480 * 2) as i64);
            assert_eq!((width, height), (800, 480));

            let mut received = 0u64;
            let mut dropped = 0u64;
            assert_eq!(mdc_mirror_stats(session, &mut received, &mut dropped), 0);
            assert_eq!(received, 1);
            assert_eq!(dropped, 0);

            // No further frame is available.
            assert_eq!(
                mdc_take_mirror_frame(
                    session,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    0
                ),
                -1
            );
            mdc_session_destroy(session);
        }
    }

    #[test]
    fn scene_set_and_clear_via_c_abi() {
        unsafe {
            let session = mdc_session_create();
            assert!(!session.is_null());
            let node = std::ffi::CString::new(r#"{"name":"root","kind":"rect"}"#).unwrap();
            assert_eq!(mdc_set_scene(session, node.as_ptr()), 0);
            assert_eq!(mdc_clear_scene(session), 0);
            let bad = std::ffi::CString::new("not json").unwrap();
            assert_eq!(mdc_set_scene(session, bad.as_ptr()), -2);
            mdc_session_destroy(session);
        }
    }

    #[test]
    fn tile_stats_are_reported_via_c_abi() {
        unsafe {
            let session = mdc_session_create();
            assert!(!session.is_null());
            let (mut tiles, mut bytes) = (7u64, 7u64);
            assert_eq!(mdc_session_tile_stats(session, &mut tiles, &mut bytes), 0);
            assert_eq!((tiles, bytes), (0, 0));
            assert_eq!(
                mdc_session_tile_stats(session, std::ptr::null_mut(), std::ptr::null_mut()),
                0
            );
            assert_eq!(
                mdc_session_tile_stats(std::ptr::null(), &mut tiles, &mut bytes),
                -1
            );
            mdc_session_destroy(session);
        }
    }

    #[test]
    fn gesture_config_round_trips_via_c_abi() {
        unsafe {
            let session = mdc_session_create();
            assert!(!session.is_null());
            // N498: the defaults are readable...
            let (mut tap, mut long, mut drag) = (0u64, 0u64, 0u16);
            assert_eq!(
                mdc_session_gesture_config(session, &mut tap, &mut long, &mut drag),
                0
            );
            assert!(tap > 0 && long > 0 && drag > 0);
            // ...and the setter replaces them.
            assert_eq!(mdc_session_set_gesture_config(session, 111, 2222, 9), 0);
            assert_eq!(
                mdc_session_gesture_config(session, &mut tap, &mut long, &mut drag),
                0
            );
            assert_eq!((tap, long, drag), (111, 2222, 9));
            // Null out-params and a null session are handled.
            assert_eq!(
                mdc_session_gesture_config(
                    session,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut()
                ),
                0
            );
            assert_eq!(
                mdc_session_set_gesture_config(std::ptr::null_mut(), 1, 1, 1),
                -1
            );
            mdc_session_destroy(session);
        }
    }

    #[test]
    fn config_round_trip_and_package_errors_via_c_abi() {
        unsafe {
            let session = mdc_session_create();
            assert!(!session.is_null());
            // N244: config round trip.
            let key = std::ffi::CString::new("brightness").unwrap();
            let value = std::ffi::CString::new("200").unwrap();
            assert_eq!(mdc_set_config(session, key.as_ptr(), value.as_ptr()), 0);
            let mut out = [0i8; 32];
            let n = mdc_get_config(session, key.as_ptr(), out.as_mut_ptr(), out.len());
            assert!(n > 0);
            let text = std::ffi::CStr::from_ptr(out.as_ptr()).to_string_lossy();
            assert_eq!(text, "200");
            // Unknown key is refused.
            let unknown = std::ffi::CString::new("nope").unwrap();
            assert_eq!(
                mdc_get_config(session, unknown.as_ptr(), out.as_mut_ptr(), out.len()),
                -3
            );
            // N243: a bad package sends (0) but never becomes ready.
            let bad = [1u8, 2, 3];
            assert_eq!(mdc_install_package(session, bad.as_ptr(), bad.len()), 0);
            assert_eq!(mdc_package_ready(session), 0);
            assert_eq!(mdc_install_package(session, std::ptr::null(), 0), -2);
            assert_eq!(
                mdc_install_package(std::ptr::null_mut(), bad.as_ptr(), bad.len()),
                -1
            );
            mdc_session_destroy(session);
        }
    }
}
