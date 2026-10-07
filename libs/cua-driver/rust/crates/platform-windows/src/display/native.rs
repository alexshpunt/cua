//! Win32 monitor metadata and GDI capture on one PMv2-aware worker thread.

use super::{DisplayReadError, DisplayTopology};
use anyhow::{bail, Result};
use cua_driver_contract::{DisplayBounds, DisplayInfo};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{BOOL, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, DISPLAY_DEVICEW, HDC, HMONITOR,
    MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Shell::GetScaleFactorForMonitor;

struct PhysicalPixels(DPI_AWARENESS_CONTEXT);
impl PhysicalPixels {
    fn enter() -> Result<Self> {
        let old =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if old.0.is_null() {
            bail!("Could not establish physical monitor coordinate space");
        }
        Ok(Self(old))
    }
}
impl Drop for PhysicalPixels {
    fn drop(&mut self) {
        unsafe {
            SetThreadDpiAwarenessContext(self.0);
        }
    }
}

fn text(value: &[u16]) -> String {
    String::from_utf16_lossy(&value[..value.iter().position(|v| *v == 0).unwrap_or(value.len())])
}
fn bounds(rect: RECT) -> Result<DisplayBounds> {
    let width = rect.right.checked_sub(rect.left).filter(|v| *v > 0);
    let height = rect.bottom.checked_sub(rect.top).filter(|v| *v > 0);
    match (width, height) {
        (Some(width), Some(height)) => Ok(DisplayBounds {
            x: rect.left,
            y: rect.top,
            width: width as u32,
            height: height as u32,
        }),
        _ => Err(DisplayReadError::InvalidTopology.into()),
    }
}

unsafe fn monitor_info(monitor: HMONITOR) -> Result<DisplayInfo> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if !GetMonitorInfoW(monitor, &mut info.monitorInfo).as_bool() {
        bail!("GetMonitorInfoW failed");
    }
    let mut paths = Vec::new();
    let mut index = 0;
    loop {
        let mut device = DISPLAY_DEVICEW {
            cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        // The device interface name is native evidence, not a guessed EDID identity.
        if !EnumDisplayDevicesW(PCWSTR(info.szDevice.as_ptr()), index, &mut device, 1).as_bool() {
            break;
        }
        let path = text(&device.DeviceID);
        if !path.is_empty() {
            paths.push(path);
        }
        index += 1;
    }
    let scale = GetScaleFactorForMonitor(monitor)
        .ok()
        .map(|scale| f64::from(scale.0) / 100.0)
        .filter(|scale| *scale > 0.0);
    Ok(DisplayInfo {
        display_id: text(&info.szDevice),
        monitor_handle: format!("0x{:x}", monitor.0 as usize),
        monitor_device_paths: paths,
        bounds: bounds(info.monitorInfo.rcMonitor)?,
        work_area: bounds(info.monitorInfo.rcWork)?,
        primary: info.monitorInfo.dwFlags & 1 != 0,
        scale_factor: scale,
        // GetDpiForMonitor is not supported for a per-monitor-aware calling thread.
        // GetDpiForWindow would describe a window, not this display.
        dpi: None,
    })
}

fn current() -> Result<DisplayTopology> {
    struct Collected {
        displays: Vec<DisplayInfo>,
        error: Option<anyhow::Error>,
    }
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let result = &mut *(data.0 as *mut Collected);
        match monitor_info(monitor) {
            Ok(display) => {
                result.displays.push(display);
                BOOL(1)
            }
            Err(error) => {
                result.error = Some(error);
                BOOL(0)
            }
        }
    }
    let mut collected = Collected {
        displays: vec![],
        error: None,
    };
    let success = unsafe {
        EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(collect),
            LPARAM(&mut collected as *mut Collected as isize),
        )
    };
    if let Some(error) = collected.error {
        return Err(error);
    }
    if !success.as_bool() {
        bail!("EnumDisplayMonitors failed");
    }
    Ok(DisplayTopology::new(collected.displays)?)
}

/// Enumerate the current connected monitors in host-global physical pixels.
pub fn enumerate() -> Result<DisplayTopology> {
    let _physical = PhysicalPixels::enter()?;
    current()
}

/// Read only the exact monitor. Drop pixels if topology changes across the capture.
pub fn capture(
    display_id: &str,
    topology_id: &str,
) -> Result<(
    DisplayInfo,
    Vec<u8>,
    cursor_overlay::capture_exclusion::AgentOverlayCapture,
)> {
    use cursor_overlay::capture_exclusion::{capture_excluding_overlays, ResidualCheck};
    let _physical = PhysicalPixels::enter()?;
    let before = current()?;
    let selected = before.select(display_id, topology_id)?.clone();
    let region = &selected.bounds;
    let (png, overlay) = capture_excluding_overlays(
        &crate::overlay::CaptureExcluder,
        |_| -> Result<(Vec<u8>, ResidualCheck)> {
            Ok((
                crate::capture::screenshot_screen_region_bytes(
                    region.x,
                    region.y,
                    region.width as i32,
                    region.height as i32,
                )?,
                ResidualCheck::Clean,
            ))
        },
    )?;
    let after = current()?;
    after.select(display_id, topology_id)?;
    let dimensions = crate::capture::png_dimensions_pub(&png)?;
    if dimensions != (region.width, region.height) {
        bail!("Selected-display capture dimensions do not match native bounds");
    }
    Ok((selected, png, overlay))
}
