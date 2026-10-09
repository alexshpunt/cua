//! Read-only retained WGC experiment. No snapshot or input authority is published.
use super::policy::{content_bytes, empty_frame_hresult, fresh_frame, qpc_100ns};
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    io::{BufRead, Read, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};
use windows::{
    core::{IInspectable, Interface},
    Foundation::{EventRegistrationToken, TypedEventHandler},
    Graphics::{
        Capture::{
            Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
            GraphicsCaptureSession,
        },
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
        SizeInt32,
    },
    Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE, HWND, WAIT_TIMEOUT},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0},
            Direct3D11::{
                D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
                D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
                D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
            },
            Dxgi::IDXGIDevice,
        },
        System::{
            Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
            Threading::{
                GetProcessTimes, OpenProcess, WaitForSingleObject,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            },
            WinRT::{
                Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
                Graphics::Capture::IGraphicsCaptureItemInterop,
                RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED,
            },
        },
        UI::{
            HiDpi::{
                AreDpiAwarenessContextsEqual, GetThreadDpiAwarenessContext,
                SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT,
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            },
            WindowsAndMessaging::{GetWindowThreadProcessId, IsIconic, IsWindow},
        },
    },
};

const FRAME_WAIT: Duration = Duration::from_millis(1500);
const IDLE_LIMIT: Duration = Duration::from_secs(5);
const LIFE_LIMIT: Duration = Duration::from_secs(300);
const MAX_REQUESTS: usize = 128;

// The Driver executable's manifest is PMv2-aware; Cargo examples have no such manifest.
// Match physical-pixel geometry on this owned worker and restore its prior thread context.
struct PhysicalPixels(DPI_AWARENESS_CONTEXT);
impl PhysicalPixels {
    fn new() -> Result<Self> {
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        ensure!(!previous.0.is_null(), "dpi_context_unavailable");
        let guard = Self(previous);
        ensure!(
            unsafe {
                AreDpiAwarenessContextsEqual(
                    GetThreadDpiAwarenessContext(),
                    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                )
            }
            .as_bool(),
            "physical_pixel_context_not_established"
        );
        Ok(guard)
    }
}
impl Drop for PhysicalPixels {
    fn drop(&mut self) {
        unsafe {
            SetThreadDpiAwarenessContext(self.0);
        }
    }
}
struct Apartment;
impl Apartment {
    fn new() -> Result<Self> {
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED)?;
        }
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            RoUninitialize();
        }
    }
}

struct Process(HANDLE);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}
struct Target {
    hwnd: HWND,
    pid: u32,
    process: Process,
    started: u64,
}
impl Target {
    fn new(pid: u32, window: u64) -> Result<Self> {
        let process = Process(unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                false,
                pid,
            )?
        });
        let started = Self::started(process.0)?;
        let target = Self {
            hwnd: HWND(window as *mut _),
            pid,
            process,
            started,
        };
        target.validate()?;
        Ok(target)
    }
    fn started(handle: HANDLE) -> Result<u64> {
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        unsafe {
            GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user)?;
        }
        Ok((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            unsafe { WaitForSingleObject(self.process.0, 0) } == WAIT_TIMEOUT,
            "target_process_exited"
        );
        ensure!(
            unsafe { IsWindow(self.hwnd) }.as_bool(),
            "target_window_closed"
        );
        let mut owner = 0;
        unsafe {
            GetWindowThreadProcessId(self.hwnd, Some(&mut owner));
        }
        ensure!(owner == self.pid, "target_owner_changed");
        let current =
            Process(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, owner)? });
        ensure!(
            Self::started(current.0)? == self.started,
            "target_process_reused"
        );
        ensure!(
            !unsafe { IsIconic(self.hwnd) }.as_bool(),
            "target_minimized"
        );
        Ok(())
    }
}

fn clock_100ns() -> Result<i64> {
    let (mut ticks, mut frequency) = (0, 0);
    unsafe {
        QueryPerformanceCounter(&mut ticks)?;
        QueryPerformanceFrequency(&mut frequency)?;
    }
    qpc_100ns(ticks, frequency).context("invalid_qpc_clock")
}
fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

struct Frame(Direct3D11CaptureFrame);
impl Drop for Frame {
    fn drop(&mut self) {
        let _ = self.0.Close();
    }
}
struct Capture {
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    closed: Arc<AtomicBool>,
    closed_token: Option<EventRegistrationToken>,
    arrived_token: Option<EventRegistrationToken>,
    arrived: mpsc::Receiver<()>,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    direct: IDirect3DDevice,
    size: SizeInt32,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    last_frame: i64,
}
impl Drop for Capture {
    fn drop(&mut self) {
        if let Some(token) = self.arrived_token.take() {
            let _ = self.pool.RemoveFrameArrived(token);
        }
        if let Some(token) = self.closed_token.take() {
            let _ = self.item.RemoveClosed(token);
        }
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}
impl Capture {
    fn new(target: &Target) -> Result<Self> {
        target.validate()?;
        let (mut device, mut context) = (None, None);
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                windows::Win32::Foundation::HMODULE(std::ptr::null_mut()),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }
        let device: ID3D11Device = device.context("missing_d3d_device")?;
        let context = context.context("missing_d3d_context")?;
        let dxgi: IDXGIDevice = device.cast()?;
        let direct: IDirect3DDevice =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? }.cast()?;
        let interop: IGraphicsCaptureItemInterop =
            windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(target.hwnd)? };
        let size = item.Size()?;
        ensure!(
            content_bytes(
                size.Width,
                size.Height,
                size.Width.max(0) as u32,
                size.Height.max(0) as u32,
                (size.Width.max(0) as u32).saturating_mul(4)
            )
            .is_some(),
            "invalid_capture_size"
        );
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &direct,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )?;
        let session = match pool.CreateCaptureSession(&item) {
            Ok(value) => value,
            Err(error) => {
                let _ = pool.Close();
                return Err(error.into());
            }
        };
        let (notify, arrived) = mpsc::sync_channel(1);
        let closed = Arc::new(AtomicBool::new(false));
        let mut capture = Self {
            item,
            pool,
            session,
            closed: closed.clone(),
            closed_token: None,
            arrived_token: None,
            arrived,
            device,
            context,
            direct,
            size,
            staging: None,
            last_frame: -1,
        };
        capture.closed_token = Some(capture.item.Closed(&TypedEventHandler::<
            GraphicsCaptureItem,
            IInspectable,
        >::new(move |_, _| {
            closed.store(true, Ordering::Release);
            Ok(())
        }))?);
        capture.arrived_token = Some(capture.pool.FrameArrived(&TypedEventHandler::<
            Direct3D11CaptureFramePool,
            IInspectable,
        >::new(move |_, _| {
            let _ = notify.try_send(());
            Ok(())
        }))?);
        let _ = capture.session.SetIsBorderRequired(false);
        let _ = capture.session.SetIsCursorCaptureEnabled(false);
        capture.session.StartCapture()?;
        Ok(capture)
    }
    fn validate(&self, target: &Target) -> Result<()> {
        ensure!(!self.closed.load(Ordering::Acquire), "capture_item_closed");
        target.validate()
    }
    fn next(&mut self, target: &Target, request_time: i64) -> Result<(Vec<u8>, u32, u32, Value)> {
        self.validate(target)?;
        let deadline = Instant::now() + FRAME_WAIT;
        let waiting = Instant::now();
        let mut discarded = 0;
        let mut recreations = 0;
        loop {
            ensure!(Instant::now() < deadline, "fresh_frame_timeout");
            self.validate(target)?;
            let mut newest: Option<(Frame, i64)> = None;
            // Drain a bounded queue, closing every rejected or replaced frame.
            for _ in 0..8 {
                let frame = match self.pool.TryGetNextFrame() {
                    Ok(value) => Frame(value),
                    Err(error) if empty_frame_hresult(error.code().0) => break,
                    Err(error) => return Err(error).context("TryGetNextFrame failed"),
                };
                let time = frame.0.SystemRelativeTime()?.Duration;
                if fresh_frame(time, request_time, self.last_frame)
                    && newest
                        .as_ref()
                        .is_none_or(|(_, newest_time)| time > *newest_time)
                {
                    newest = Some((frame, time));
                } else {
                    discarded += 1;
                }
            }
            if let Some((frame, time)) = newest {
                let content = frame.0.ContentSize()?;
                if content != self.size {
                    ensure!(recreations < 3, "capture_resize_did_not_settle");
                    ensure!(
                        content_bytes(
                            content.Width,
                            content.Height,
                            content.Width.max(0) as u32,
                            content.Height.max(0) as u32,
                            (content.Width.max(0) as u32).saturating_mul(4)
                        )
                        .is_some(),
                        "invalid_frame_content"
                    );
                    drop(frame);
                    self.pool.Recreate(
                        &self.direct,
                        DirectXPixelFormat::B8G8R8A8UIntNormalized,
                        2,
                        content,
                    )?;
                    self.size = content;
                    self.staging = None;
                    recreations += 1;
                    continue;
                }
                let wait_ms = ms(waiting);
                let reading = Instant::now();
                let surface = frame.0.Surface()?;
                let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
                let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                unsafe {
                    texture.GetDesc(&mut desc);
                }
                let width = content.Width as u32;
                let height = content.Height as u32;
                ensure!(
                    content_bytes(
                        content.Width,
                        content.Height,
                        desc.Width,
                        desc.Height,
                        desc.Width.saturating_mul(4)
                    )
                    .is_some(),
                    "invalid_frame_extent"
                );
                if self
                    .staging
                    .as_ref()
                    .is_none_or(|(_, w, h)| *w != desc.Width || *h != desc.Height)
                {
                    desc.Usage = D3D11_USAGE_STAGING;
                    desc.BindFlags = 0;
                    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
                    desc.MiscFlags = 0;
                    let mut staging = None;
                    unsafe {
                        self.device
                            .CreateTexture2D(&desc, None, Some(&mut staging))?;
                    }
                    self.staging = Some((
                        staging.context("missing_staging_texture")?,
                        desc.Width,
                        desc.Height,
                    ));
                }
                let staging = &self.staging.as_ref().context("missing_staging")?.0;
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                unsafe {
                    self.context.CopyResource(staging, &texture);
                    self.context
                        .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
                }
                // Always unmap, even if bounds checks or allocation fail.
                let copy = (|| -> Result<Vec<u8>> {
                    let length = content_bytes(
                        content.Width,
                        content.Height,
                        desc.Width,
                        desc.Height,
                        mapped.RowPitch,
                    )
                    .context("invalid_mapped_extent")?;
                    ensure!(!mapped.pData.is_null(), "null_mapped_data");
                    let mut pixels = Vec::new();
                    pixels.try_reserve_exact(length)?;
                    pixels.resize(length, 0);
                    for row in 0..height as usize {
                        unsafe {
                            std::ptr::copy_nonoverlapping(
                                (mapped.pData as *const u8).add(row * mapped.RowPitch as usize),
                                pixels.as_mut_ptr().add(row * width as usize * 4),
                                width as usize * 4,
                            );
                        }
                    }
                    Ok(pixels)
                })();
                unsafe {
                    self.context.Unmap(staging, 0);
                }
                let pixels = copy?;
                drop(texture);
                drop(access);
                drop(surface);
                drop(frame);
                self.validate(target)?;
                let completed = clock_100ns()?;
                ensure!(
                    completed >= time,
                    "incompatible_frame_clock: request_100ns={request_time}, frame_100ns={time}, copy_completed_100ns={completed}, ahead_100ns={}",
                    i128::from(time) - i128::from(completed)
                );
                self.last_frame = time;
                return Ok((
                    pixels,
                    width,
                    height,
                    json!({"wait_ms":wait_ms,"readback_ms":ms(reading),"frame_age_at_copy_ms":(completed-time) as f64/10_000.0,"frame_after_request_ms":(time-request_time) as f64/10_000.0,"discarded":discarded,"recreations":recreations}),
                ));
            }
            // The event wakes this wait. A short bounded fallback also covers a missed wake.
            let remaining = deadline.saturating_duration_since(Instant::now());
            let _ = self
                .arrived
                .recv_timeout(remaining.min(Duration::from_millis(5)));
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: u32,
    route: String,
    output: String,
}

/// Run one explicitly selected read-only target. EOF, errors, idle or count/time limits close all WGC resources.
pub(super) fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 3,
        "Usage: persistent-wgc-probe PID HWND OWNED_OUTPUT_DIRECTORY"
    );
    let _physical_pixels = PhysicalPixels::new()?;
    let _apartment = Apartment::new()?;
    let target = Target::new(args[0].parse()?, args[1].parse()?)?;
    let directory = std::fs::canonicalize(&args[2])?;
    ensure!(directory.is_dir(), "output_directory_missing");
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut reader = std::io::stdin().lock();
        loop {
            let mut line = String::new();
            let result = (&mut reader).take(4097).read_line(&mut line);
            if !matches!(result, Ok(n) if n > 0 && n <= 4096) {
                break;
            }
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let lifetime = Instant::now();
    let mut capture = None;
    let mut count = 0;
    println!(
        "{}",
        json!({"ready":true,"input_authority":false,"dpi":"per_monitor_v2","max_requests":MAX_REQUESTS,"idle_ms":IDLE_LIMIT.as_millis(),"lifetime_ms":LIFE_LIMIT.as_millis()})
    );
    std::io::stdout().flush()?;
    while count < MAX_REQUESTS && lifetime.elapsed() < LIFE_LIMIT {
        let line = match receiver
            .recv_timeout(IDLE_LIMIT.min(LIFE_LIMIT.saturating_sub(lifetime.elapsed())))
        {
            Ok(value) => value,
            Err(_) => break,
        };
        let request: Request = serde_json::from_str(&line)?;
        ensure!(
            ["printwindow", "wgc", "close"].contains(&request.route.as_str()),
            "unknown_capture_route"
        );
        if request.route == "close" {
            break;
        }
        ensure!(
            !request.output.is_empty()
                && request.output.len() <= 80
                && request
                    .output
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
            "invalid_output_name"
        );
        count += 1;
        let started = Instant::now();
        let request_time = clock_100ns()?;
        let result = (|| -> Result<(Vec<u8>, Value)> {
            target.validate()?;
            if request.route == "printwindow" {
                let (png, width, height) = platform_windows::capture::screenshot_window_overview(
                    target.hwnd.0 as u64,
                    500,
                )?;
                target.validate()?;
                return Ok((
                    png,
                    json!({"native_width":width,"native_height":height,"route":"guarded_printwindow","total_ms":ms(started),"frame_timestamp_available":false}),
                ));
            }
            let mut setup_ms = 0.0;
            if capture.is_none() {
                let setup = Instant::now();
                capture = Some(Capture::new(&target).context("WGC setup")?);
                setup_ms = ms(setup);
            }
            let (pixels, width, height, mut stats) = capture
                .as_mut()
                .context("missing_capture")?
                .next(&target, request_time)
                .context("WGC fresh frame")?;
            let encoding = Instant::now();
            let png = cua_driver_core::image_utils::encode_bgra_to_png_resized(
                &pixels, width, height, 500,
            )?;
            stats["processing_ms"] = json!(ms(encoding));
            stats["setup_ms"] = json!(setup_ms);
            stats["total_ms"] = json!(ms(started));
            stats["native_width"] = json!(width);
            stats["native_height"] = json!(height);
            stats["route"] = json!("persistent_wgc");
            Ok((png, stats))
        })();
        match result {
            Ok((png, stats)) => {
                let file = directory.join(format!("{}.png", request.output));
                ensure!(!file.exists(), "output_already_exists");
                std::fs::write(file, png)?;
                println!("{}", json!({"id":request.id,"ok":true,"stats":stats}));
                std::io::stdout().flush()?;
            }
            Err(error) => {
                drop(capture.take());
                println!(
                    "{}",
                    json!({"id":request.id,"ok":false,"error":format!("{error:#}"),"capture_dropped":true})
                );
                std::io::stdout().flush()?;
                return Ok(());
            }
        }
    }
    drop(capture);
    println!("{}", json!({"closed":true,"requests":count}));
    std::io::stdout().flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_pixel_context_is_scoped_to_the_probe_worker() {
        let previous = unsafe { GetThreadDpiAwarenessContext() };
        {
            let _guard = PhysicalPixels::new().unwrap();
            assert!(unsafe {
                AreDpiAwarenessContextsEqual(
                    GetThreadDpiAwarenessContext(),
                    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                )
            }
            .as_bool());
        }
        assert!(
            unsafe { AreDpiAwarenessContextsEqual(GetThreadDpiAwarenessContext(), previous) }
                .as_bool()
        );
    }
    #[test]
    fn windows_binding_reports_a_successful_null_interface_as_an_empty_error() {
        let result: windows::core::Result<Direct3D11CaptureFrame> =
            unsafe { windows::core::Type::from_abi(std::ptr::null_mut()) };
        let error = result.unwrap_err();
        assert_eq!(error.code().0, 0);
        assert!(super::super::policy::empty_frame_hresult(error.code().0));
    }
}
