//! Session/window ownership for the opt-in Windows capture worker.
//! Native resources stay on one MTA thread. No pixels are retained in this registry.
use anyhow::{ensure, Result};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

#[cfg(target_os = "windows")]
mod native;
mod policy;

#[cfg(target_os = "windows")]
pub(crate) fn mapped_dimensions(window: u64) -> Result<(u32, u32)> {
    let bounds = native::frame_bounds(windows::Win32::Foundation::HWND(window as *mut _))?;
    let width = u32::try_from(
        bounds
            .2
            .checked_sub(bounds.0)
            .ok_or_else(|| anyhow::anyhow!("wgc_invalid_frame_bounds"))?,
    )?;
    let height = u32::try_from(
        bounds
            .3
            .checked_sub(bounds.1)
            .ok_or_else(|| anyhow::anyhow!("wgc_invalid_frame_bounds"))?,
    )?;
    policy::mapped_crop(bounds, width, height)
        .ok_or_else(|| anyhow::anyhow!("wgc_frame_geometry_unmapped"))
}

/// An explicit backend never widens the source or silently becomes a UI-only read.
pub(crate) fn requested(
    value: Option<&serde_json::Value>,
    image: bool,
    session: Option<&str>,
) -> Result<bool> {
    match value {
        None => Ok(false),
        Some(serde_json::Value::String(value)) if value == "default" => Ok(false),
        Some(serde_json::Value::String(value)) if value == "wgc" => {
            ensure!(image, "wgc_requires_screenshot");
            ensure!(
                session.is_some_and(|session| !session.is_empty()),
                "wgc_runtime_session_required"
            );
            Ok(true)
        }
        _ => anyhow::bail!("invalid_capture_backend"),
    }
}
const MAX_ACTORS: usize = 8;
const IDLE: Duration = Duration::from_secs(5);
const REQUEST_LIMIT: Duration = Duration::from_millis(1500);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct Target {
    pub session: String,
    pub pid: u32,
    pub window: u64,
}

/// The frame is in the existing bitmap action domain, not arbitrary WGC texture space.
#[derive(Debug)]
pub(crate) struct Frame {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub metadata: serde_json::Value,
}

struct Control {
    cancelled: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    deadline: Instant,
}
impl Control {
    fn check(&self) -> Result<()> {
        ensure!(
            !self.cancelled.load(Ordering::Acquire),
            "wgc_request_cancelled"
        );
        ensure!(!self.closed.load(Ordering::Acquire), "wgc_session_closed");
        ensure!(Instant::now() < self.deadline, "wgc_frame_timeout");
        Ok(())
    }
}

/// Native resource seam: creation, reads and destruction all run on the same worker.
trait Backend {
    fn read(&mut self, max_dimension: u32, control: &Control) -> Result<Frame>;
}
type Factory = fn(&Target) -> Result<Box<dyn Backend>>;

struct Request {
    max_dimension: u32,
    cancelled: Arc<AtomicBool>,
    reply: oneshot::Sender<Result<Frame>>,
    deadline: Instant,
}
struct Worker {
    requests: mpsc::SyncSender<Request>,
    closed: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
    }
}
struct Pending(Arc<AtomicBool>);
impl Drop for Pending {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

pub(crate) struct Manager {
    workers: Mutex<HashMap<Target, Worker>>,
    factory: Factory,
}
impl Manager {
    #[cfg(target_os = "windows")]
    pub(crate) fn new() -> Self {
        Self::with_factory(native::create)
    }
    fn with_factory(factory: Factory) -> Self {
        Self {
            workers: Mutex::new(HashMap::new()),
            factory,
        }
    }

    pub(crate) async fn capture(&self, target: Target, max_dimension: u32) -> Result<Frame> {
        ensure!(!target.session.is_empty(), "wgc_runtime_session_required");
        let cancelled = Arc::new(AtomicBool::new(false));
        let _pending = Pending(cancelled.clone());
        let (reply, response) = oneshot::channel();
        {
            let mut workers = self.workers.lock().unwrap();
            // Keep stopping workers in the bound until their resources actually drop.
            workers.retain(|_, worker| !worker.thread.is_finished());
            if !workers.contains_key(&target) {
                ensure!(workers.len() < MAX_ACTORS, "wgc_resource_limit");
                workers.insert(target.clone(), spawn(target.clone(), self.factory)?);
            }
            let worker = workers.get(&target).unwrap();
            ensure!(!worker.closed.load(Ordering::Acquire), "wgc_session_closed");
            worker
                .requests
                .try_send(Request {
                    max_dimension,
                    cancelled,
                    reply,
                    deadline: Instant::now() + REQUEST_LIMIT,
                })
                .map_err(|_| anyhow::anyhow!("wgc_worker_busy_or_closed"))?;
        }
        response
            .await
            .map_err(|_| anyhow::anyhow!("wgc_worker_closed"))?
    }

    pub(crate) fn close_session(&self, session: &str) {
        for (target, worker) in self.workers.lock().unwrap().iter() {
            if target.session == session {
                worker.closed.store(true, Ordering::Release);
            }
        }
    }
}
fn spawn(target: Target, factory: Factory) -> Result<Worker> {
    let (requests, incoming) = mpsc::sync_channel::<Request>(1);
    let closed = Arc::new(AtomicBool::new(false));
    let stopping = closed.clone();
    let thread = thread::Builder::new()
        .name("cua-wgc".into())
        .spawn(move || {
            let mut backend: Option<Box<dyn Backend>> = None;
            let mut last_request = Instant::now();
            loop {
                if stopping.load(Ordering::Acquire) || last_request.elapsed() >= IDLE {
                    break;
                }
                let request = match incoming.recv_timeout(Duration::from_millis(5)) {
                    Ok(request) => request,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                last_request = Instant::now();
                let control = Control {
                    cancelled: request.cancelled,
                    closed: stopping.clone(),
                    deadline: request.deadline,
                };
                let result = (|| {
                    control.check()?;
                    let reused = backend.is_some();
                    let setup = Instant::now();
                    if !reused {
                        backend = Some(factory(&target)?);
                    }
                    let setup_ms = if reused {
                        0.0
                    } else {
                        setup.elapsed().as_secs_f64() * 1000.0
                    };
                    control.check()?;
                    let mut frame = backend
                        .as_mut()
                        .unwrap()
                        .read(request.max_dimension, &control)?;
                    control.check()?;
                    frame.metadata["resources_reused"] = serde_json::json!(reused);
                    frame.metadata["setup_ms"] = serde_json::json!(setup_ms);
                    Ok(frame)
                })();
                let failed = result.is_err();
                let abandoned = request.reply.send(result).is_err();
                if failed || abandoned {
                    break;
                }
            }
            // Drop the native resource before publishing the finished/stopped state.
            drop(backend);
            stopping.store(true, Ordering::Release);
        })?;
    Ok(Worker {
        requests,
        closed,
        thread,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn explicit_capture_does_not_fall_back_or_allocate_for_ui_only() {
        use serde_json::json;
        assert!(!requested(None, false, None).unwrap());
        assert!(!requested(Some(&json!("default")), false, None).unwrap());
        assert!(requested(Some(&json!("wgc")), true, Some("session")).unwrap());
        assert_eq!(
            requested(Some(&json!("wgc")), false, Some("session"))
                .unwrap_err()
                .to_string(),
            "wgc_requires_screenshot"
        );
        assert_eq!(
            requested(Some(&json!("wgc")), true, None)
                .unwrap_err()
                .to_string(),
            "wgc_runtime_session_required"
        );
        assert_eq!(
            requested(Some(&json!(7)), true, Some("session"))
                .unwrap_err()
                .to_string(),
            "invalid_capture_backend"
        );
    }
    struct IdleFixture;
    impl Backend for IdleFixture {
        fn read(&mut self, dimension: u32, control: &Control) -> Result<Frame> {
            while dimension == 999 {
                control.check()?;
                thread::park_timeout(Duration::from_millis(1));
            }
            Ok(Frame {
                png: vec![1],
                width: 1,
                height: 1,
                metadata: serde_json::json!({}),
            })
        }
    }
    fn idle_fixture(_: &Target) -> Result<Box<dyn Backend>> {
        Ok(Box::new(IdleFixture))
    }

    #[tokio::test]
    async fn actors_have_a_real_resource_bound_idle_expiry_and_frame_deadline() {
        let manager = Manager::with_factory(idle_fixture);
        for index in 0..MAX_ACTORS {
            let frame = manager
                .capture(target(&index.to_string()), 500)
                .await
                .unwrap();
            assert_eq!(frame.metadata["resources_reused"], false);
        }
        assert_eq!(
            manager
                .capture(target("overflow"), 500)
                .await
                .unwrap_err()
                .to_string(),
            "wgc_resource_limit"
        );
        let warm = manager.capture(target("0"), 500).await.unwrap();
        assert_eq!(warm.metadata["resources_reused"], true);
        assert_eq!(warm.metadata["setup_ms"], 0.0);
        tokio::time::sleep(IDLE + Duration::from_millis(100)).await;
        let reopened = manager.capture(target("0"), 500).await.unwrap();
        assert_eq!(reopened.metadata["resources_reused"], false);
        assert_eq!(
            manager
                .capture(target("0"), 999)
                .await
                .unwrap_err()
                .to_string(),
            "wgc_frame_timeout"
        );
    }
    static CREATED: AtomicUsize = AtomicUsize::new(0);
    static DROPPED: AtomicUsize = AtomicUsize::new(0);
    struct Fixture;
    impl Drop for Fixture {
        fn drop(&mut self) {
            DROPPED.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl Backend for Fixture {
        fn read(&mut self, dimension: u32, control: &Control) -> Result<Frame> {
            while dimension == 999 {
                control.check()?;
                thread::yield_now();
            }
            Ok(Frame {
                png: vec![1],
                width: 1,
                height: 1,
                metadata: serde_json::json!({}),
            })
        }
    }
    fn fixture(_: &Target) -> Result<Box<dyn Backend>> {
        CREATED.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Fixture))
    }
    fn target(session: &str) -> Target {
        Target {
            session: session.into(),
            pid: 1,
            window: 2,
        }
    }

    #[tokio::test]
    async fn retained_resources_are_session_owned_and_cancelled_reads_drop_them() {
        let manager = Arc::new(Manager::with_factory(fixture));
        manager.capture(target("first"), 500).await.unwrap();
        manager.capture(target("first"), 500).await.unwrap();
        assert_eq!(CREATED.load(Ordering::SeqCst), 1);
        manager.capture(target("second"), 500).await.unwrap();
        assert_eq!(CREATED.load(Ordering::SeqCst), 2);
        manager.close_session("first");
        manager.capture(target("second"), 500).await.unwrap();
        let working = manager.clone();
        let pending = tokio::spawn(async move { working.capture(target("second"), 999).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        pending.abort();
        assert!(pending.await.unwrap_err().is_cancelled());
        let until = Instant::now() + Duration::from_secs(1);
        while DROPPED.load(Ordering::SeqCst) < 2 && Instant::now() < until {
            tokio::task::yield_now().await;
        }
        assert_eq!(DROPPED.load(Ordering::SeqCst), 2);
    }
}
