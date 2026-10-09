//! C ABI: opaque MdcSession over FakeDevice + Session.
use mdc_core::{ConnectionState, Frame, Session};
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
/// Currently a no-op placeholder for ABI symmetry; only NULL is valid until helpers allocate.
#[no_mangle]
pub unsafe extern "C" fn mdc_free(ptr: *mut std::ffi::c_void) {
    let _ = ptr;
}
