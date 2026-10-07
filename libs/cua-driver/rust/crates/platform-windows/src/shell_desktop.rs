//! Documented Windows shell desktop queries. Reads never activate or move a window or desktop.

use cua_driver_contract::{DesktopQueryError, DesktopQueryErrorCode, VirtualDesktopMembership};
use std::{
    marker::PhantomData,
    rc::Rc,
    time::{SystemTime, UNIX_EPOCH},
};
use windows::{
    core::HRESULT,
    Win32::{
        Foundation::HWND,
        System::Com::{
            CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
        },
        UI::{
            Shell::{IVirtualDesktopManager, VirtualDesktopManager},
            WindowsAndMessaging::{GetWindowThreadProcessId, IsWindow},
        },
    },
};

// COM objects must drop before this thread-owned initialization guard.
struct Apartment {
    initialized: bool,
    _thread: PhantomData<Rc<()>>,
}
impl Apartment {
    fn new() -> Result<Self, DesktopQueryError> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // An existing STA is usable; do not change or uninitialize someone else's apartment.
        if hr.is_err() && hr != HRESULT(0x80010106_u32 as i32) {
            return Err(error(
                DesktopQueryErrorCode::ComInitializationFailed,
                Some(hr.0),
            ));
        }
        Ok(Self {
            initialized: hr.is_ok(),
            _thread: PhantomData,
        })
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        if self.initialized {
            unsafe { CoUninitialize() };
        }
    }
}

/// One thread-owned query context can sample several windows without a process-wide COM cache.
pub(crate) struct Query {
    manager: Result<IVirtualDesktopManager, DesktopQueryError>,
    _apartment: Option<Apartment>,
}
impl Query {
    pub(crate) fn new() -> Self {
        match Apartment::new() {
            Ok(apartment) => Self {
                manager: unsafe { CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL) }
                    .map_err(|e| {
                        error(DesktopQueryErrorCode::ManagerUnavailable, Some(e.code().0))
                    }),
                _apartment: Some(apartment),
            },
            Err(error) => Self {
                manager: Err(error),
                _apartment: None,
            },
        }
    }

    /// Query this exact HWND/process pair. Method failures are independent and never guessed from visibility.
    pub(crate) fn read(&self, pid: u32, window_id: u64) -> VirtualDesktopMembership {
        let hwnd = HWND(window_id as usize as *mut core::ffi::c_void);
        if !same_window(hwnd, pid) {
            return unavailable(DesktopQueryErrorCode::WindowUnavailable, None);
        }
        let manager = match &self.manager {
            Ok(manager) => manager,
            Err(error) => {
                return VirtualDesktopMembership::from_queries(
                    Err(error.clone()),
                    Err(error.clone()),
                    now_ms(),
                )
            }
        };
        let desktop_id = unsafe { manager.GetWindowDesktopId(hwnd) }
            .map(|guid| format!("{guid:?}").to_ascii_lowercase())
            .map_err(|e| {
                error(
                    DesktopQueryErrorCode::DesktopIdQueryFailed,
                    Some(e.code().0),
                )
            });
        let current = unsafe { manager.IsWindowOnCurrentVirtualDesktop(hwnd) }
            .map(|value| value.as_bool())
            .map_err(|e| {
                error(
                    DesktopQueryErrorCode::CurrentDesktopQueryFailed,
                    Some(e.code().0),
                )
            });
        if !same_window(hwnd, pid) {
            return unavailable(DesktopQueryErrorCode::WindowUnavailable, None);
        }
        VirtualDesktopMembership::from_queries(desktop_id, current, now_ms())
    }
}

fn same_window(hwnd: HWND, pid: u32) -> bool {
    let mut owner = 0;
    unsafe {
        IsWindow(hwnd).as_bool()
            && GetWindowThreadProcessId(hwnd, Some(&mut owner)) != 0
            && owner == pid
    }
}
fn error(code: DesktopQueryErrorCode, hresult: Option<i32>) -> DesktopQueryError {
    DesktopQueryError { code, hresult }
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
/// Preserve a failed worker or missing window as two explicit unknown outcomes.
pub(crate) fn unavailable(
    code: DesktopQueryErrorCode,
    hresult: Option<i32>,
) -> VirtualDesktopMembership {
    let error = error(code, hresult);
    VirtualDesktopMembership::from_queries(Err(error.clone()), Err(error), now_ms())
}
