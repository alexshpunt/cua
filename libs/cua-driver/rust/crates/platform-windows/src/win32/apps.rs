//! Enumerate running processes on Windows.
//!
//! Uses CreateToolhelp32Snapshot / Process32FirstW / Process32NextW (simpler
//! and more portable than NtQuerySystemInformation or EnumProcesses+psapi).

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

pub use crate::process_selection::ProcessInfo;
use crate::process_selection::{descendants_from_processes, related_processes_from_processes};

/// Read one process's executable basename without scanning the process table.
/// This is best-effort display metadata, not proof of process or window lifetime.
pub(crate) fn process_name(pid: u32) -> Option<String> {
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    let path = unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let result = process_image_path(process);
        let _ = CloseHandle(process);
        result.ok()?
    };
    let name = std::path::Path::new(&path)
        .file_name()?
        .to_string_lossy()
        .into_owned();
    (!name.trim().is_empty()).then_some(name)
}

/// Query a borrowed process handle. The caller owns the handle and closes it on every result.
fn process_image_path(
    process: windows::Win32::Foundation::HANDLE,
) -> windows::core::Result<String> {
    use windows::core::PWSTR;
    use windows::Win32::System::Threading::{QueryFullProcessImageNameW, PROCESS_NAME_FORMAT};

    let mut buffer = vec![0_u16; 32_768];
    let mut length = buffer.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )?;
    }
    Ok(String::from_utf16_lossy(&buffer[..length as usize]))
}

/// Return all running processes (pid, parent_pid, executable name).
pub fn list_processes() -> Vec<ProcessInfo> {
    let mut result = Vec::new();
    unsafe {
        let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(h) => h,
            Err(_) => return result,
        };

        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                let name = decode_wstr(&entry.szExeFile);
                result.push(ProcessInfo {
                    pid: entry.th32ProcessID,
                    parent_pid: entry.th32ParentProcessID,
                    name,
                });
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    result
}

fn decode_wstr(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// Return all transitive descendants of `root_pid` (BFS through the process
/// tree). Includes processes that may have been spawned *after* `root_pid`
/// itself exited — useful for tracking launcher-stub chains where the
/// originally-launched binary re-execs into another process and exits (GIMP's
/// `gimp-3.exe` → `gimp-3.2.exe`; LibreOffice's `swriter.exe` → `soffice.bin`).
///
/// The result is in arrival order, which on Windows tends to correlate with
/// process-creation order — useful when picking the "main" descendant to
/// query for windows. Always includes `root_pid` itself first (even if it's
/// no longer alive, callers handle the empty-windows case the same way).
pub fn list_descendants(root_pid: u32) -> Vec<u32> {
    let all = list_processes();
    descendants_from_processes(root_pid, &all)
}

/// Return the launch receipt root and its transitive descendants, root first.
/// Executable-name similarity does not establish launch ancestry.
pub fn related_processes(root_pid: u32, exe_basename: &str) -> Vec<u32> {
    related_processes_from_processes(root_pid, exe_basename, &list_processes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_name_matches_the_current_executable_without_a_path() {
        let executable = std::env::current_exe().unwrap();
        let expected = executable.file_name().unwrap().to_string_lossy();
        assert_eq!(
            process_name(std::process::id()).as_deref(),
            Some(expected.as_ref())
        );
    }

    #[test]
    fn process_name_does_not_fabricate_a_name_for_an_unqueryable_pid() {
        assert_eq!(process_name(0), None);
    }

    #[test]
    fn process_image_path_preserves_access_denied_without_query_rights() {
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};

        // A real handle to this test process, with no permission to query its image.
        // Do not change the process DACL, token privileges or any other process.
        let handle =
            unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, std::process::id()).unwrap() };
        let result = process_image_path(handle);
        unsafe { CloseHandle(handle).unwrap() };
        assert_eq!(result.unwrap_err().code().0 as u32, 0x8007_0005);
    }
}
