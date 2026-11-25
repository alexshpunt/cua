// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Aleksandr Shpuntenko

//! One heartbeat file per process and one renderer lease per interactive desktop.
//! These files contain visual metadata only and are never consulted for input.

use super::*;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::Duration;
use windows::Win32::System::StationsAndDesktops::{
    GetProcessWindowStation, GetUserObjectInformationW, UOI_NAME,
};
use windows::Win32::System::SystemInformation::GetTickCount64;

const MAX_LOCAL: usize = 16;
const MAX_ROWS: usize = 128;
const MAX_BYTES: u64 = 128 * 1024;
const HEARTBEAT_MS: u64 = 500;

#[derive(Deserialize, Serialize)]
struct Snapshot {
    seen: u64,
    rows: Vec<Agent>,
}

#[derive(Default)]
struct Publications {
    sessions: BTreeMap<String, Session>,
    retired: Vec<Agent>,
    next_id: u64,
}

impl Publications {
    fn project(&mut self, now: u64) -> Vec<Agent> {
        self.retired.retain_mut(|row| {
            expire(row, now, now);
            !row.recent.is_empty()
        });
        self.sessions
            .values_mut()
            .map(|session| session.project(now))
            .filter(|row| row.active || !row.recent.is_empty())
            .chain(self.retired.iter().cloned())
            .collect()
    }

    fn retire(&mut self, owner: &str, now: u64) {
        if let Some(mut session) = self.sessions.remove(owner) {
            session.finish(Outcome::Interrupted, now);
            let row = session.project(now);
            if !row.recent.is_empty() {
                self.retired.insert(0, row);
                self.retired.truncate(MAX_LOCAL);
            }
        }
    }
}
struct Publisher {
    id: String,
    folder: PathBuf,
    path: PathBuf,
    staging: PathBuf,
    publications: Mutex<Publications>,
    visible_here: AtomicBool,
    io: Mutex<()>,
    wake: mpsc::SyncSender<()>,
}

static PUBLISHER: OnceLock<Option<Publisher>> = OnceLock::new();

fn now() -> u64 {
    unsafe { GetTickCount64() }
}

fn folder() -> Option<PathBuf> {
    let desktop = crate::diagnostics::desktop_state();
    let session = desktop.session_id.filter(|id| *id != 0)?;
    if !desktop
        .thread_desktop_name
        .as_deref()?
        .eq_ignore_ascii_case("Default")
    {
        return None;
    }
    let station = unsafe { GetProcessWindowStation() }.ok()?;
    let mut name = [0u16; 64];
    unsafe {
        GetUserObjectInformationW(
            windows::Win32::Foundation::HANDLE(station.0),
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            std::mem::size_of_val(&name) as u32,
            None,
        )
        .ok()?;
    }
    let end = name.iter().position(|c| *c == 0)?;
    if !String::from_utf16_lossy(&name[..end]).eq_ignore_ascii_case("WinSta0") {
        return None;
    }
    Some(
        PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
            .join("Cua")
            .join("desktop-activity-v1")
            .join(session.to_string()),
    )
}

fn init() -> Option<Publisher> {
    if !cua_driver_core::desktop_activity::enabled() {
        return None;
    }
    let folder = folder()?;
    fs::create_dir_all(&folder).ok()?;
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).ok()?;
    let id = format!("{}-{:032x}", std::process::id(), u128::from_ne_bytes(nonce));
    let (wake, rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("cua-desktop-activity".into())
        .spawn(move || {
            if let Some(publisher) = PUBLISHER.wait().as_ref() {
                publisher.run(rx);
            }
        })
        .ok()?;
    Some(Publisher {
        path: folder.join(format!("{id}.json")),
        staging: folder.join(format!("{id}.tmp")),
        id,
        folder,
        publications: Mutex::default(),
        visible_here: AtomicBool::new(false),
        io: Mutex::default(),
        wake,
    })
}

fn read_snapshot(path: &Path) -> Option<Snapshot> {
    let mut buffer = Vec::new();
    File::open(path)
        .ok()?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut buffer)
        .ok()?;
    if buffer.len() as u64 > MAX_BYTES {
        return None;
    }
    serde_json::from_slice(&buffer).ok()
}

impl Publisher {
    fn publish(&self) -> std::io::Result<()> {
        let _io = self
            .io
            .lock()
            .map_err(|_| std::io::Error::other("activity publisher lock poisoned"))?;
        let seen = now();
        let rows = self
            .publications
            .lock()
            .map_err(|_| std::io::Error::other("activity publisher state poisoned"))?
            .project(seen);
        self.visible_here.store(
            rows.iter()
                .any(|row| row.panel || (row.active && (row.outline || row.point))),
            Ordering::Relaxed,
        );
        if rows.is_empty() {
            let _ = fs::remove_file(&self.path);
            return Ok(());
        }
        let data = serde_json::to_vec(&Snapshot { seen, rows })?;
        let mut file = File::create(&self.staging)?;
        file.write_all(&data)?;
        drop(file);
        fs::rename(&self.staging, &self.path)
    }

    fn rows(&self, now: u64) -> Vec<Agent> {
        let mut rows = Vec::new();
        let Ok(files) = fs::read_dir(&self.folder) else {
            return rows;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json")
                || !file.file_type().is_ok_and(|kind| kind.is_file())
            {
                continue;
            }
            let Some(snapshot) = read_snapshot(&path) else {
                continue;
            };
            let observed = now.max(self::now());
            if observed.saturating_sub(snapshot.seen) >= RECENT_MS + STALE_MS
                || snapshot.seen.saturating_sub(observed) >= STALE_MS
            {
                // Live publishers replace heartbeat files. Never remove our own
                // or a newer one written between the read and cleanup.
                if path != self.path
                    && read_snapshot(&path).is_some_and(|current| current.seen == snapshot.seen)
                {
                    let _ = fs::remove_file(path);
                }
                continue;
            }
            for mut row in snapshot.rows.into_iter().take(2 * MAX_LOCAL) {
                expire(&mut row, snapshot.seen, observed);
                if row.active || !row.recent.is_empty() {
                    rows.push(row);
                }
            }
        }
        rows.sort_by(|a, b| b.active.cmp(&a.active).then_with(|| a.id.cmp(&b.id)));
        rows.truncate(MAX_ROWS);
        rows
    }

    fn run(&self, rx: mpsc::Receiver<()>) {
        let mut renderer: Option<super::native::Renderer> = None;
        let mut lease: Option<File> = None;
        let mut last_publish = 0;
        let mut dirty = false;
        let mut last_lease_attempt = 0;
        loop {
            let interval = if self.visible_here.load(Ordering::Relaxed) {
                70
            } else {
                HEARTBEAT_MS
            };
            let wake = match rx.recv_timeout(Duration::from_millis(interval)) {
                Ok(()) => true,
                Err(mpsc::RecvTimeoutError::Timeout) => false,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            let now = now();
            dirty |= wake;
            if (dirty && now.saturating_sub(last_publish) >= 50)
                || now.saturating_sub(last_publish) >= HEARTBEAT_MS
            {
                let _ = self.publish();
                dirty = false;
                last_publish = now;
            }
            if cua_driver_core::human_stop::stopped() {
                if let Ok(mut state) = self.publications.lock() {
                    for session in state.sessions.values_mut() {
                        session.finish(Outcome::Interrupted, now);
                    }
                }
                let _ = self.publish();
                break;
            }
            if !self.visible_here.load(Ordering::Relaxed) {
                renderer = None;
                lease = None;
                continue;
            }
            if lease.is_none() && now.saturating_sub(last_lease_attempt) >= 250 {
                last_lease_attempt = now;
                if let Ok(file) = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(self.folder.join("renderer.lock"))
                {
                    if file.try_lock().is_ok() {
                        renderer = super::native::Renderer::new();
                        if renderer.is_some() {
                            lease = Some(file);
                        }
                    }
                }
            }
            if let Some(renderer) = &mut renderer {
                let rows = self.rows(now);
                renderer.render(&rows, rows.iter().any(|row| row.panel));
            }
        }
        // Drop every native surface before another process can acquire the lease.
        drop(renderer);
        drop(lease);
    }
}

pub(crate) fn apply(owner: &str, update: Update) -> Result<bool, &'static str> {
    let publisher = PUBLISHER
        .get_or_init(init)
        .as_ref()
        .ok_or("desktop_activity_unavailable")?;
    let mut state = publisher
        .publications
        .lock()
        .map_err(|_| "desktop_activity_unavailable")?;
    if !state.sessions.contains_key(owner) {
        if !matches!(update, Update::Configure { .. }) {
            return Ok(false);
        }
        if state.sessions.len() >= MAX_LOCAL {
            return Err("desktop_activity_busy");
        }
        let next = format!("{}/{:02}", publisher.id, state.next_id);
        state.next_id += 1;
        state.sessions.insert(owner.to_owned(), Session::new(next));
    }
    let synchronous = matches!(
        update,
        Update::Begin { .. } | Update::End { .. } | Update::Configure { .. }
    );
    let changed = state
        .sessions
        .get_mut(owner)
        .expect("this owner was prepared")
        .apply(update, now())?;
    drop(state);
    if changed {
        if synchronous {
            let _ = publisher.publish();
        }
        let _ = publisher.wake.try_send(());
    }
    Ok(changed)
}

pub(crate) fn end(owner: &str) {
    let Some(publisher) = PUBLISHER.get().and_then(Option::as_ref) else {
        return;
    };
    if let Ok(mut state) = publisher.publications.lock() {
        state.retire(owner, now());
    }
    let _ = publisher.publish();
    let _ = publisher.wake.try_send(());
}

fn note(shared: bool, position: Option<(i32, i32)>) {
    let Some(call) = cua_driver_core::desktop_activity::current() else {
        return;
    };
    let Some(publisher) = PUBLISHER.get().and_then(Option::as_ref) else {
        return;
    };
    if let Ok(mut state) = publisher.publications.lock() {
        if let Some(session) = state.sessions.get_mut(&call.owner) {
            session.note(&call, shared, position);
        }
    }
    let _ = publisher.wake.try_send(());
}

pub(crate) fn note_shared() {
    note(true, None);
}
pub(crate) fn note_direct() {
    note(false, None);
}

/// Publish only when Windows reports the intended point in physical desktop pixels.
pub(crate) fn note_pointer_arrival(x: i32, y: i32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetPhysicalCursorPos;
    if cua_driver_core::desktop_activity::current().is_none() {
        return;
    }
    let mut point = POINT::default();
    if unsafe { GetPhysicalCursorPos(&mut point) }.is_ok() && (point.x, point.y) == (x, y) {
        note(true, Some((x, y)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_a_transport_frees_its_slot_and_keeps_only_bounded_recent_activity() {
        let mut state = Publications::default();
        for i in 0..MAX_LOCAL + 3 {
            let mut session = Session::new(format!("agent-{i}"));
            session
                .apply(
                    Update::Configure {
                        label: format!("Agent {i}"),
                        panel: true,
                        outline: true,
                        point: false,
                    },
                    1,
                )
                .unwrap();
            session
                .apply(
                    Update::Begin {
                        intent: "Check a scratch window".into(),
                    },
                    2,
                )
                .unwrap();
            state.sessions.insert("transport".into(), session);
            state.retire("transport", 3);
            assert!(state.sessions.is_empty());
        }
        let rows = state.project(4);
        assert_eq!(rows.len(), MAX_LOCAL);
        assert!(rows.iter().all(|row| {
            !row.active
                && row.intent.is_empty()
                && row.action.is_none()
                && row.recent[0].outcome == Outcome::Interrupted
        }));
        assert_eq!(rows.last().unwrap().id, "agent-3");
        assert!(state.project(RECENT_MS + 3).is_empty());
    }
}
