// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Aleksandr Shpuntenko

//! Windows held by one agent on a shared desktop.
//!
//! Several agents can drive the same desktop, each through its own driver process. An agent claims a
//! window to keep the others out of it: the claiming process holds an exclusive lock on
//! `<window_id>-<pid>.lock` in `cua-driver-window-claims` under the user's temporary directory, and other
//! driver processes refuse input into that window while the lock is held. The operating system releases
//! the lock when the process exits, so a closed or crashed agent frees its windows.
//!
//! The lock file is named by the window and the process that owns it, so a window handle that Windows
//! reuses for another process's window is a different claim. A sidecar `<window_id>-<pid>.json`,
//! written by the holder after it takes the lock, names the holder for refusals: Windows blocks reads of
//! a locked file's bytes from other handles.

use std::collections::HashMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The refusal code for input into a window another agent holds.
pub const CLAIMED_CODE: &str = "window_claimed";
/// The holder named when the holder's sidecar is missing or unreadable.
pub const UNKNOWN_HOLDER: &str = "another agent";
/// How long a claim waits for a lock that another process may hold only for a probe.
const CLAIM_WAIT: Duration = Duration::from_millis(200);
/// How long a probe waits before it trusts a held lock: another process's probe holds it only briefly.
const PROBE_WAIT: Duration = Duration::from_millis(100);
const POLL: Duration = Duration::from_millis(5);

fn directory() -> PathBuf {
    std::env::temp_dir().join("cua-driver-window-claims")
}

fn key(window_id: u64, pid: u32) -> String {
    format!("{window_id}-{pid}")
}

/// Claims this process holds, by key, with the window each one is for.
fn held() -> &'static Mutex<HashMap<String, (u64, File)>> {
    static HELD: OnceLock<Mutex<HashMap<String, (u64, File)>>> = OnceLock::new();
    HELD.get_or_init(Mutex::default)
}

fn lock_held() -> std::sync::MutexGuard<'static, HashMap<String, (u64, File)>> {
    held().lock().unwrap_or_else(|poison| poison.into_inner())
}

/// The holder another process's sidecar names for a claim.
fn read_holder(directory: &Path, key: &str) -> String {
    std::fs::read(directory.join(format!("{key}.json")))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value.get("holder")?.as_str().map(str::to_owned))
        .filter(|holder| !holder.is_empty())
        .unwrap_or_else(|| UNKNOWN_HOLDER.to_owned())
}

/// Write the sidecar through a temporary file, so a reader never sees half of it.
fn write_holder(directory: &Path, key: &str, window_id: u64, pid: u32, holder: &str) {
    let staged = directory.join(format!("{key}.json.{}", std::process::id()));
    let body = serde_json::json!({ "window_id": window_id, "pid": pid, "holder": holder });
    if std::fs::write(&staged, body.to_string()).is_ok()
        && std::fs::rename(&staged, directory.join(format!("{key}.json"))).is_err()
    {
        let _ = std::fs::remove_file(&staged);
    }
}

/// Claim window `window_id` of process `pid` for this process. Claiming a window this process already
/// holds succeeds again and renames its holder. `Err` names the agent that holds it.
pub fn claim(window_id: u64, pid: u32, holder: &str) -> Result<(), String> {
    claim_in(&directory(), window_id, pid, holder)
}

fn claim_in(directory: &Path, window_id: u64, pid: u32, holder: &str) -> Result<(), String> {
    let key = key(window_id, pid);
    let mut held = lock_held();
    if held.contains_key(&key) {
        write_holder(directory, &key, window_id, pid, holder);
        return Ok(());
    }
    std::fs::create_dir_all(directory).map_err(|error| {
        format!(
            "the window claim folder {} is unavailable: {error}",
            directory.display()
        )
    })?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(format!("{key}.lock")))
        .map_err(|error| format!("the window claim file is unavailable: {error}"))?;
    let deadline = Instant::now() + CLAIM_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => std::thread::sleep(POLL),
            Err(TryLockError::WouldBlock) => return Err(read_holder(directory, &key)),
            Err(TryLockError::Error(error)) => {
                return Err(format!(
                    "the window claim file could not be locked: {error}"
                ))
            }
        }
    }
    write_holder(directory, &key, window_id, pid, holder);
    held.insert(key, (window_id, file));
    Ok(())
}

/// Release every claim this process holds on window `window_id`. `true` when there was one.
pub fn release(window_id: u64) -> bool {
    release_in(&directory(), window_id)
}

fn release_in(directory: &Path, window_id: u64) -> bool {
    let mut held = lock_held();
    let keys: Vec<String> = held
        .iter()
        .filter(|(_, (window, _))| *window == window_id)
        .map(|(key, _)| key.clone())
        .collect();
    for key in &keys {
        // The sidecar goes first, while the lock still keeps other claimers out. The lock file stays:
        // removing a file another process may have open for a probe could split one claim in two.
        let _ = std::fs::remove_file(directory.join(format!("{key}.json")));
        // Closing the handle releases the lock.
        held.remove(key);
    }
    !keys.is_empty()
}

/// Release every claim this process holds.
pub fn release_all() {
    let directory = directory();
    let mut held = lock_held();
    for key in held.keys() {
        let _ = std::fs::remove_file(directory.join(format!("{key}.json")));
    }
    held.clear();
}

/// Whether this process holds window `window_id` of process `pid`.
pub fn held_here(window_id: u64, pid: u32) -> bool {
    lock_held().contains_key(&key(window_id, pid))
}

/// The agent that holds window `window_id` of process `owner_pid`, when it is another process. Blocks for
/// up to 100 ms while the window is held, so call it off the async runtime.
pub fn held_by_other(window_id: u64, owner_pid: u32) -> Option<String> {
    held_by_other_in(&directory(), window_id, owner_pid)
}

fn held_by_other_in(directory: &Path, window_id: u64, owner_pid: u32) -> Option<String> {
    let key = key(window_id, owner_pid);
    if lock_held().contains_key(&key) {
        return None;
    }
    // No file: nobody ever claimed this window.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.join(format!("{key}.lock")))
        .ok()?;
    let deadline = Instant::now() + PROBE_WAIT;
    loop {
        match file.try_lock() {
            // Free: dropping the handle releases the probe's lock at once.
            Ok(()) => return None,
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => std::thread::sleep(POLL),
            Err(TryLockError::WouldBlock) => return Some(read_holder(directory, &key)),
            Err(TryLockError::Error(_)) => return None,
        }
    }
}

/// The refusal text for input into a window another agent holds.
pub fn refusal_message(window_id: u64, holder: &str) -> String {
    format!(
        "{CLAIMED_CODE}: window {window_id} is held by {holder}; nothing was sent. \
         Work in another window, or wait until that agent releases it."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Another process's claim: its own handle on the lock file and its sidecar.
    fn foreign_claim(directory: &Path, window_id: u64, pid: u32, holder: &str) -> File {
        std::fs::create_dir_all(directory).unwrap();
        let key = key(window_id, pid);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join(format!("{key}.lock")))
            .unwrap();
        file.try_lock().unwrap();
        write_holder(directory, &key, window_id, pid, holder);
        file
    }

    #[test]
    fn a_claim_of_another_process_refuses_and_names_its_holder() {
        let directory = tempfile::tempdir().unwrap();
        let other = foreign_claim(directory.path(), 9101, 42, "agent Alpha");
        assert_eq!(
            held_by_other_in(directory.path(), 9101, 42).as_deref(),
            Some("agent Alpha")
        );
        assert_eq!(
            claim_in(directory.path(), 9101, 42, "agent Beta").unwrap_err(),
            "agent Alpha"
        );
        // The process exits: the operating system releases its lock.
        drop(other);
        assert_eq!(held_by_other_in(directory.path(), 9101, 42), None);
        claim_in(directory.path(), 9101, 42, "agent Beta").unwrap();
        release_in(directory.path(), 9101);
    }

    #[test]
    fn a_reused_window_handle_of_another_process_is_not_held() {
        let directory = tempfile::tempdir().unwrap();
        let _other = foreign_claim(directory.path(), 9102, 42, "agent Alpha");
        assert_eq!(held_by_other_in(directory.path(), 9102, 43), None);
        assert_eq!(held_by_other_in(directory.path(), 9103, 42), None);
    }

    #[test]
    fn own_claims_are_not_refused_and_release_frees_them() {
        let directory = tempfile::tempdir().unwrap();
        claim_in(directory.path(), 9104, 42, "agent Alpha").unwrap();
        // Claiming again renames the holder.
        claim_in(directory.path(), 9104, 42, "agent Alpha 2").unwrap();
        assert!(lock_held().contains_key(&key(9104, 42)));
        assert_eq!(held_by_other_in(directory.path(), 9104, 42), None);
        // Another process sees this process's claim.
        let probe = OpenOptions::new()
            .write(true)
            .open(directory.path().join("9104-42.lock"))
            .unwrap();
        assert!(matches!(probe.try_lock(), Err(TryLockError::WouldBlock)));
        assert_eq!(read_holder(directory.path(), "9104-42"), "agent Alpha 2");
        assert!(release_in(directory.path(), 9104));
        assert!(!release_in(directory.path(), 9104));
        probe.try_lock().unwrap();
        assert!(!directory.path().join("9104-42.json").exists());
    }

    #[test]
    fn a_held_lock_without_a_sidecar_names_an_unknown_holder() {
        let directory = tempfile::tempdir().unwrap();
        let other = foreign_claim(directory.path(), 9105, 42, "agent Alpha");
        std::fs::remove_file(directory.path().join("9105-42.json")).unwrap();
        assert_eq!(
            held_by_other_in(directory.path(), 9105, 42).as_deref(),
            Some(UNKNOWN_HOLDER)
        );
        drop(other);
    }

    #[test]
    fn a_probe_of_an_unclaimed_window_creates_nothing() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(held_by_other_in(directory.path(), 9106, 42), None);
        assert!(std::fs::read_dir(directory.path())
            .unwrap()
            .next()
            .is_none());
    }
}
