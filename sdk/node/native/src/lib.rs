use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::sync::Mutex;

#[derive(Default)]
struct State {
    disposed: bool,
    last_frame: Option<Vec<u8>>,
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

    /// Copies bytes before returning, so the JS Buffer may be reused immediately.
    #[napi]
    pub fn send_frame(&self, bytes: Buffer) -> Result<u32> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        state.last_frame = Some(bytes.to_vec());
        Ok(state.last_frame.as_ref().map_or(0, Vec::len) as u32)
    }

    #[napi]
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
        state.disposed = true;
        state.last_frame = None;
        Ok(())
    }
}
