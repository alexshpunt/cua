// SPDX-License-Identifier: FSL-1.1-MIT
// Copyright (c) 2026 Cua AI, Inc.

//! Window capture by polling `PrintWindow(PW_RENDERFULLCONTENT)`, for windows
//! that Windows.Graphics.Capture cannot open.
//!
//! WGC needs the per-user capture service (`CaptureService_<luid>`). Headless
//! Windows sessions, such as the "utility" sessions of MXC isolated agent
//! sessions, start no per-user services, so `CreateForWindow` fails there with
//! `ERROR_SERVICE_DOES_NOT_EXIST`, and with no display device the screen DC is
//! black as well. `PrintWindow` asks the window to paint itself into a memory
//! DC, which works in those sessions; it is the same route the cua-driver
//! screenshot tools use.
//!
//! The loop paints the window at most `max_fps` times a second and publishes a
//! frame only when its pixels changed, so an idle window costs one paint per
//! tick and no encode. Frames are cropped to `DWMWA_EXTENDED_FRAME_BOUNDS`
//! when DWM reports them, which keeps frame pixels in the space the cua-driver
//! pixel tools and `frame_point_to_screen` use; without DWM the frame is the
//! whole `GetWindowRect`, which is also the origin that mapping falls back to.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cua_media_protocol::SurfaceGeometry;
use cua_spacesd_provider_api::{
    CaptureEvent, CaptureSink, OwnedFrame, PixelFormat, ProviderError, ProviderErrorCode,
};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC,
    HGDIOBJ, RGBQUAD,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, IsIconic, IsWindow};

use crate::windows_support;

/// `PW_RENDERFULLCONTENT`: include DirectComposition content.
const PW_RENDERFULLCONTENT: u32 = 2;
const MONITOR_INTERVAL: Duration = Duration::from_millis(200);

fn capture_error(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorCode::CaptureFailed, message.into())
}

/// The part of the window bitmap that becomes the frame, in window-rect pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    /// Size of the `GetWindowRect` bitmap PrintWindow paints.
    window: (i32, i32),
    /// Frame origin inside that bitmap and frame size.
    crop: (i32, i32, i32, i32),
}

impl Layout {
    /// `window` and `frame` are screen rectangles (left, top, right, bottom):
    /// the window rect and, when DWM reports them, the extended frame bounds.
    fn new(window: (i32, i32, i32, i32), frame: Option<(i32, i32, i32, i32)>) -> Option<Self> {
        let (left, top, right, bottom) = window;
        let (width, height) = (right.checked_sub(left)?, bottom.checked_sub(top)?);
        if width <= 0 || height <= 0 {
            return None;
        }
        let crop = frame
            .and_then(|(fl, ft, fr, fb)| {
                let x = fl.max(left) - left;
                let y = ft.max(top) - top;
                let w = fr.min(right) - left - x;
                let h = fb.min(bottom) - top - y;
                (w > 0 && h > 0).then_some((x, y, w, h))
            })
            .unwrap_or((0, 0, width, height));
        Some(Self {
            window: (width, height),
            crop,
        })
    }

    fn frame_size(&self) -> (u32, u32) {
        (self.crop.2 as u32, self.crop.3 as u32)
    }
}

fn layout(hwnd: HWND) -> Option<Layout> {
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect).ok()? };
    let mut bounds = RECT::default();
    let frame = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut bounds as *mut RECT as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
    }
    .ok()
    .map(|()| (bounds.left, bounds.top, bounds.right, bounds.bottom));
    Layout::new((rect.left, rect.top, rect.right, rect.bottom), frame)
}

/// A memory DC with a bitmap the size of the window rect.
struct Surface {
    memory: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    layout: Layout,
    window_pixels: Vec<u8>,
}

impl Surface {
    fn new(layout: Layout) -> Result<Self, ProviderError> {
        let (width, height) = layout.window;
        unsafe {
            let screen = GetDC(HWND(std::ptr::null_mut()));
            if screen.is_invalid() {
                return Err(capture_error("GetDC(NULL) failed"));
            }
            let memory = CreateCompatibleDC(screen);
            let bitmap = CreateCompatibleBitmap(screen, width, height);
            ReleaseDC(HWND(std::ptr::null_mut()), screen);
            if memory.is_invalid() || bitmap.is_invalid() {
                if !bitmap.is_invalid() {
                    let _ = DeleteObject(bitmap);
                }
                if !memory.is_invalid() {
                    let _ = DeleteDC(memory);
                }
                return Err(capture_error(format!(
                    "could not create a {width}x{height} memory bitmap"
                )));
            }
            let previous = SelectObject(memory, bitmap);
            Ok(Self {
                memory,
                bitmap,
                previous,
                layout,
                window_pixels: Vec::new(),
            })
        }
    }

    /// Paint the window and return the cropped frame as packed opaque BGRA.
    fn grab(&mut self, hwnd: HWND) -> Result<Vec<u8>, ProviderError> {
        let (width, height) = self.layout.window;
        unsafe {
            if !PrintWindow(hwnd, self.memory, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)).as_bool() {
                return Err(capture_error("PrintWindow failed"));
            }
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    // Negative height: top-down rows.
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                bmiColors: [RGBQUAD::default(); 1],
            };
            self.window_pixels
                .resize(width as usize * height as usize * 4, 0);
            let rows = GetDIBits(
                self.memory,
                self.bitmap,
                0,
                height as u32,
                Some(self.window_pixels.as_mut_ptr().cast()),
                &mut info,
                DIB_RGB_COLORS,
            );
            if rows != height {
                return Err(capture_error(format!(
                    "GetDIBits copied {rows} of {height} rows"
                )));
            }
        }
        Ok(crop_opaque(
            &self.window_pixels,
            width as usize,
            self.layout.crop,
        ))
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.memory, self.previous);
            let _ = DeleteObject(self.bitmap);
            let _ = DeleteDC(self.memory);
        }
    }
}

/// Copy the crop rectangle out of a packed BGRA bitmap `stride_px` wide and
/// make every pixel opaque: GDI leaves alpha undefined.
fn crop_opaque(pixels: &[u8], stride_px: usize, (x, y, w, h): (i32, i32, i32, i32)) -> Vec<u8> {
    let (x, y, w, h) = (x as usize, y as usize, w as usize, h as usize);
    let mut out = Vec::with_capacity(w * h * 4);
    for row in y..y + h {
        let start = (row * stride_px + x) * 4;
        out.extend_from_slice(&pixels[start..start + w * 4]);
    }
    for pixel in out.chunks_exact_mut(4) {
        pixel[3] = 0xff;
    }
    out
}

/// Capture loop on the provider's capture thread. Reports setup through
/// `ready`: an error names both why WGC could not open the window and why
/// PrintWindow could not capture it.
pub(crate) fn run(
    hwnd_value: u64,
    max_fps: u16,
    stop: Arc<AtomicBool>,
    sink: Arc<dyn CaptureSink>,
    native_geometry: Arc<dyn Fn(u32, u32) + Send + Sync>,
    ready: std::sync::mpsc::Sender<Result<(), ProviderError>>,
    wgc_error: ProviderError,
) {
    let hwnd = HWND(hwnd_value as *mut _);
    let fail = |reason: String| {
        capture_error(format!(
            "Windows.Graphics.Capture could not open the window ({}); PrintWindow capture failed: {reason}",
            wgc_error.message
        ))
    };
    let Some(first_layout) = layout(hwnd) else {
        let _ = ready.send(Err(fail("the window has no size".into())));
        return;
    };
    let mut surface = match Surface::new(first_layout) {
        Ok(surface) => surface,
        Err(error) => {
            let _ = ready.send(Err(fail(error.message)));
            return;
        }
    };
    let mut last: Arc<[u8]> = match surface.grab(hwnd) {
        Ok(frame) => Arc::from(frame),
        Err(error) => {
            let _ = ready.send(Err(fail(error.message)));
            return;
        }
    };
    let (width, height) = first_layout.frame_size();
    native_geometry(width, height);
    let _ = ready.send(Ok(()));

    let epoch = Instant::now();
    let interval = Duration::from_micros(1_000_000 / u64::from(max_fps.clamp(1, 60)));
    let publish = |bytes: Arc<[u8]>, (width, height): (u32, u32), at: Instant| {
        sink.on_event(CaptureEvent::Frame(OwnedFrame {
            bytes,
            format: PixelFormat::Bgra8,
            width_px: width,
            height_px: height,
            bytes_per_row: Some(width * 4),
            capture_timestamp_us: at.duration_since(epoch).as_micros() as u64,
            encode_duration_us: None,
            codec_epoch: 1,
            keyframe: true,
        }));
    };
    publish(last.clone(), first_layout.frame_size(), epoch);

    let mut title = crate::windows_capture::window_title(hwnd);
    let mut suspended = false;
    let mut last_monitor = Instant::now();
    let mut next = Instant::now() + interval;
    while !stop.load(Ordering::Acquire) {
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        }
        next = (next + interval).max(Instant::now());
        if stop.load(Ordering::Acquire) {
            return;
        }

        if last_monitor.elapsed() >= MONITOR_INTERVAL {
            last_monitor = Instant::now();
            if unsafe { !IsWindow(hwnd).as_bool() } {
                sink.on_event(CaptureEvent::Closed);
                return;
            }
            let iconic = unsafe { IsIconic(hwnd).as_bool() };
            if iconic != suspended {
                suspended = iconic;
                sink.on_event(if iconic {
                    CaptureEvent::Suspended("minimized".into())
                } else {
                    CaptureEvent::Resumed
                });
            }
            let current = crate::windows_capture::window_title(hwnd);
            if current != title {
                title = current.clone();
                sink.on_event(CaptureEvent::TitleChanged(current));
            }
        }
        if suspended {
            continue;
        }

        let Some(current) = layout(hwnd) else {
            continue;
        };
        if current != surface.layout {
            let resized = current.frame_size() != surface.layout.frame_size();
            match Surface::new(current) {
                Ok(replacement) => surface = replacement,
                Err(_) => {
                    sink.on_event(CaptureEvent::Suspended("capture_reset_failed".into()));
                    return;
                }
            }
            if resized {
                let (width, height) = current.frame_size();
                native_geometry(width, height);
                sink.on_event(CaptureEvent::GeometryChanged(SurfaceGeometry {
                    width_px: width,
                    height_px: height,
                    scale_factor: windows_support::window_scale_factor(hwnd_value),
                }));
            }
        }
        let captured = Instant::now();
        match surface.grab(hwnd) {
            Ok(frame) if frame.as_slice() != &*last => {
                last = Arc::from(frame);
                publish(last.clone(), surface.layout.frame_size(), captured);
            }
            Ok(_) => {}
            Err(_) => {
                sink.on_event(CaptureEvent::Suspended("frame_copy_failed".into()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{crop_opaque, Layout};

    #[test]
    fn frame_is_cropped_to_the_extended_frame_bounds() {
        // Window rect with 7 px invisible resize borders left, right and bottom.
        let layout = Layout::new((100, 50, 914, 657), Some((107, 50, 907, 650))).unwrap();
        assert_eq!(layout.window, (814, 607));
        assert_eq!(layout.crop, (7, 0, 800, 600));
        assert_eq!(layout.frame_size(), (800, 600));
    }

    #[test]
    fn without_dwm_bounds_the_frame_is_the_window_rect() {
        let layout = Layout::new((0, 0, 768, 558), None).unwrap();
        assert_eq!(layout.crop, (0, 0, 768, 558));
    }

    #[test]
    fn bounds_outside_the_window_rect_fall_back_to_the_window_rect() {
        let layout = Layout::new((0, 0, 100, 100), Some((200, 200, 300, 300))).unwrap();
        assert_eq!(layout.crop, (0, 0, 100, 100));
        assert!(Layout::new((10, 10, 10, 40), None).is_none());
    }

    #[test]
    fn cropped_pixels_are_packed_and_opaque() {
        // 3x2 bitmap, pixel value = index, alpha 0.
        let pixels: Vec<u8> = (0..6u8).flat_map(|i| [i, i, i, 0]).collect();
        let out = crop_opaque(&pixels, 3, (1, 0, 2, 2));
        assert_eq!(
            out,
            vec![1, 1, 1, 255, 2, 2, 2, 255, 4, 4, 4, 255, 5, 5, 5, 255]
        );
    }
}
