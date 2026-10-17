//! Readonly Windows viewport diagnostics for one explicitly authorized PID/HWND.
//!
//! Build: cargo build --locked -p platform-windows --example browser_viewport_diagnostic
//! Run: browser_viewport_diagnostic.exe --readonly-approved <pid> <hwnd-decimal>
//!
//! Uses native Win32/UIA reads only. It does not launch a browser, start a Driver
//! session, call CDP, capture pixels or dispatch input. The caller owns permission
//! and selected-tab/document evidence. Results never qualify a viewport or mapping.
//! A five-second process deadline ends a hung UIA provider with exit 124 and an
//! explicit incomplete result; it is not a successful empty inventory.

#[cfg(target_os = "windows")]
mod viewport_diagnostic;

#[cfg(target_os = "windows")]
fn main() -> anyhow::Result<()> {
    use serde_json::json;
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };

    let args: Vec<_> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        args.len() == 3 && args[0] == "--readonly-approved",
        "Usage: browser_viewport_diagnostic.exe --readonly-approved <pid> <hwnd-decimal>"
    );
    let pid: u32 = args[1].parse()?;
    let window: u64 = args[2].parse()?;
    anyhow::ensure!(
        pid > 0 && pid <= i32::MAX as u32 && window > 0 && usize::try_from(window).is_ok(),
        "Invalid explicit PID/HWND"
    );
    // This affects this short-lived probe process only. inspect checks the actual
    // thread context, including when an existing executable manifest wins.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let duration = Duration::from_millis(viewport_diagnostic::DEADLINE_MS);
    let deadline = Instant::now() + duration;
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(viewport_diagnostic::native::inspect(pid, window, deadline));
    });
    match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(Ok(report)) => println!("{}", serde_json::to_string_pretty(&report)?),
        Ok(Err(error)) => {
            println!(
                "{}",
                json!({"format": "native_viewport_diagnostic_v1", "readonly": true,
                "status": "refused", "complete": false, "pid": pid, "hwnd": window, "error": error,
                "native_mapping_qualified": false})
            );
            std::process::exit(2);
        }
        Err(error) => {
            println!(
                "{}",
                json!({"format": "native_viewport_diagnostic_v1", "readonly": true,
                "status": "incomplete", "complete": false, "pid": pid, "hwnd": window,
                "error": error.to_string(), "deadline_ms": viewport_diagnostic::DEADLINE_MS,
                "native_mapping_qualified": false})
            );
            // No input/session/capture cleanup can be stranded: there is none.
            // Ending the process also stops any outstanding native read worker.
            std::process::exit(124);
        }
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("browser_viewport_diagnostic is Windows-only; no native query was attempted")
}
