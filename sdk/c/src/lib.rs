//! C ABI: opaque MdcSession over FakeDevice or a TCP simulator peer.
use mdc_core::{ConnectionState, DeviceManager, Frame, ManagerEvent, Session, Tile};
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
}

fn pump(session: &mut MdcSession) -> Result<(), ()> {
    if let Some(device) = session.device.as_mut() {
        device.poll().map_err(|_| ())?;
    }
    session.session.poll().map_err(|_| ())?;
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
    let (host, device) = FakeDevice::pair(BoardProfile::default());
    let manager = Arc::new(Mutex::new(DeviceManager::new()));
    let mut session = Session::new(Box::new(host) as Box<dyn Transport>);
    session.set_manager(manager.clone());
    session.connect().map_err(|_| ())?;
    let mut held = MdcSession {
        session,
        device: Some(device),
        manager,
        queued: Vec::new(),
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

/// # Safety
/// Returned pointer must be freed with [`mdc_session_destroy`].
#[no_mangle]
pub extern "C" fn mdc_session_create() -> *mut MdcSession {
    match open_sim() {
        Ok(s) => Box::into_raw(Box::new(s)),
        Err(()) => ptr::null_mut(),
    }
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
    let (code, id, phase_text, px, py) = match event {
        ManagerEvent::Input { device_id, event } => (1, device_id, event.phase, event.x, event.y),
        ManagerEvent::Error { device_id, message } => (2, device_id, message, 0, 0),
        ManagerEvent::Connected(id) | ManagerEvent::Added(id) => (3, id, String::new(), 0, 0),
        ManagerEvent::Disconnected(id) | ManagerEvent::Removed(id) => (4, id, String::new(), 0, 0),
        ManagerEvent::EndpointSwitched { device_id } => (5, device_id, String::new(), 0, 0),
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
