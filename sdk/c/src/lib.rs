//! C ABI: opaque MdcSession over FakeDevice + Session.
use mdc_core::{ConnectionState, Frame, Session, Tile};
use mdc_simulator::{BoardProfile, FakeDevice};
use mdc_transport::MemoryLink;
use std::os::raw::{c_char, c_int};
use std::ptr;

pub struct MdcSession {
    session: Session<MemoryLink>,
    device: FakeDevice,
}

fn open_default() -> Result<MdcSession, ()> {
    let (host, mut device) = FakeDevice::pair(BoardProfile::default());
    let mut session = Session::new(host);
    session.connect().map_err(|_| ())?;
    device.poll().map_err(|_| ())?;
    if session.poll().map_err(|_| ())? != ConnectionState::Ready {
        return Err(());
    }
    Ok(MdcSession { session, device })
}

/// # Safety
/// Returned pointer must be freed with [`mdc_session_destroy`].
#[no_mangle]
pub extern "C" fn mdc_session_create() -> *mut MdcSession {
    match open_default() {
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
    let w = s.device.profile().width;
    let h = s.device.profile().height;
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
            let _ = s.device.poll();
            let _ = s.session.poll();
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
            let _ = s.device.poll();
            let _ = s.session.poll();
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
    (*session).device.current_frame_id()
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
    *width = s.device.profile().width;
    *height = s.device.profile().height;
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
            mdc_session_destroy(session);
        }
    }
}
