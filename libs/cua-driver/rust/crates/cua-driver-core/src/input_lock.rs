//! One action at a time on a shared desktop.
//!
//! Several agents can drive the same desktop, each through its own driver process. When
//! `CUA_SHARED_INPUT_LOCK=1` is set, every tool that is not read-only takes an exclusive lock on a file in
//! the user's temporary directory before it runs and releases it when it returns, so a typed string, a drag
//! or a timeline of one agent is never interleaved with another agent's input. Nothing waits for the human:
//! the lock only orders agents. The operating system releases the lock when a process exits, so a crashed
//! driver cannot block the others.
//!
//! Calls within one process share the lock: they are already ordered by the transport, and a batch that
//! dispatches its steps through the registry must not wait for itself.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The refusal code when another agent's input did not finish in time.
pub const BUSY_CODE: &str = "input_busy";
/// How long a call waits for another agent's action before it refuses.
pub const WAIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(10);

#[derive(Default)]
struct Holder {
    file: Option<File>,
    turns: usize,
}

fn holder() -> &'static Mutex<Holder> {
    static HOLDER: OnceLock<Mutex<Holder>> = OnceLock::new();
    HOLDER.get_or_init(Mutex::default)
}

pub fn configured() -> bool {
    static CONFIGURED: OnceLock<bool> = OnceLock::new();
    *CONFIGURED
        .get_or_init(|| std::env::var("CUA_SHARED_INPUT_LOCK").is_ok_and(|value| value == "1"))
}

fn path() -> PathBuf {
    std::env::temp_dir().join("cua-driver-shared-input.lock")
}

/// This process's hold on the shared lock; the last turn to drop releases it.
pub struct InputTurn(());

impl Drop for InputTurn {
    fn drop(&mut self) {
        let mut holder = holder()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        holder.turns = holder.turns.saturating_sub(1);
        if holder.turns == 0 {
            // Closing the handle releases the lock.
            holder.file = None;
        }
    }
}

enum Attempt {
    Taken(InputTurn),
    Busy,
}

fn try_take(path: &PathBuf) -> std::io::Result<Attempt> {
    let mut holder = holder()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if holder.turns > 0 {
        holder.turns += 1;
        return Ok(Attempt::Taken(InputTurn(())));
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => {
            holder.file = Some(file);
            holder.turns = 1;
            Ok(Attempt::Taken(InputTurn(())))
        }
        Err(TryLockError::WouldBlock) => Ok(Attempt::Busy),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

/// Wait for the shared lock. `Ok(None)` when the lock is not configured. A stop by the human ends the wait.
pub async fn acquire() -> Result<Option<InputTurn>, String> {
    if !configured() {
        return Ok(None);
    }
    acquire_at(&path(), WAIT).await.map(Some)
}

async fn acquire_at(path: &PathBuf, wait: Duration) -> Result<InputTurn, String> {
    let deadline = Instant::now() + wait;
    loop {
        crate::human_stop::check()?;
        match try_take(path) {
            Ok(Attempt::Taken(turn)) => return Ok(turn),
            Ok(Attempt::Busy) if Instant::now() < deadline => tokio::time::sleep(POLL).await,
            Ok(Attempt::Busy) => {
                return Err(format!(
                "{BUSY_CODE}: another agent's input on this desktop did not finish within {} s; \
                     nothing was sent",
                wait.as_secs()
            ))
            }
            Err(error) => {
                return Err(format!(
                "{BUSY_CODE}: the shared input lock {} is unavailable: {error}; nothing was sent",
                path.display()
            ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn turns_nest_in_one_process_and_release_on_the_last_drop() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("lock");
        let outer = acquire_at(&path, Duration::from_millis(50)).await.unwrap();
        let inner = acquire_at(&path, Duration::from_millis(50)).await.unwrap();
        // Another handle stands for another process.
        let other = OpenOptions::new().write(true).open(&path).unwrap();
        assert!(matches!(other.try_lock(), Err(TryLockError::WouldBlock)));
        drop(inner);
        assert!(matches!(other.try_lock(), Err(TryLockError::WouldBlock)));
        drop(outer);
        other.try_lock().unwrap();
        // While the other holds it, a call waits and then refuses.
        let error = acquire_at(&path, Duration::from_millis(30))
            .await
            .err()
            .unwrap();
        assert!(error.starts_with("input_busy:"), "{error}");
        other.unlock().unwrap();
        drop(acquire_at(&path, Duration::from_millis(30)).await.unwrap());
    }
}
