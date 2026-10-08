//! Timing-only diagnostic module. Never installed as a production runtime.
//! Only fixed labels and timings are retained; no request arguments or pixels.
use serde::Serialize;
use std::sync::{Arc, Mutex, atomic::{AtomicBool, AtomicU64, Ordering}};
use std::time::Instant;

static ACTIVE: Mutex<Option<Arc<Profile>>> = Mutex::new(None);
static NEXT: AtomicU64 = AtomicU64::new(1);
const MAX_EVENTS: usize = 256;

#[derive(Serialize)]
struct Event { name: &'static str, start_us: u64, duration_us: u64 }
struct Profile {
    request: u64, start: Instant, events: Mutex<Vec<Event>>,
    concurrent: AtomicBool, truncated: AtomicBool,
}
impl Profile {
    fn record(&self, name: &'static str, start: Instant, end: Instant) {
        let mut events = self.events.lock().unwrap();
        if events.len() < MAX_EVENTS {
            events.push(Event { name, start_us: start.saturating_duration_since(self.start).as_micros() as u64,
                duration_us: end.saturating_duration_since(start).as_micros() as u64 });
        } else { self.truncated.store(true, Ordering::Relaxed); }
    }
}

/// Own one parsed observation request through its normal stdout response.
/// Concurrent requests are marked invalid for attribution, never blocked.
pub struct Root { profile: Arc<Profile> }
impl Root {
    pub fn begin(start: Instant) -> Self {
        let profile = Arc::new(Profile { request: NEXT.fetch_add(1, Ordering::Relaxed), start,
            events: Mutex::new(Vec::new()), concurrent: AtomicBool::new(false), truncated: AtomicBool::new(false) });
        let mut active = ACTIVE.lock().unwrap();
        if let Some(previous) = active.as_ref() {
            previous.concurrent.store(true, Ordering::Relaxed);
            profile.concurrent.store(true, Ordering::Relaxed);
        }
        *active = Some(profile.clone());
        Self { profile }
    }
    pub fn record(&self, name: &'static str, start: Instant, end: Instant) {
        self.profile.record(name, start, end);
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let end = Instant::now();
        {
            let mut active = ACTIVE.lock().unwrap();
            if active.as_ref().is_some_and(|p| Arc::ptr_eq(p, &self.profile)) { *active = None; }
        }
        #[derive(Serialize)]
        struct Report<'a> {
            schema: u8, request: u64, total_us: u64, concurrent: bool, truncated: bool,
            events: &'a [Event],
        }
        let events = self.profile.events.lock().unwrap();
        let report = Report { schema: 1, request: self.profile.request,
            total_us: end.saturating_duration_since(self.profile.start).as_micros() as u64,
            concurrent: self.profile.concurrent.load(Ordering::Relaxed),
            truncated: self.profile.truncated.load(Ordering::Relaxed), events: &events };
        // One bounded stderr line after normal stdout delivery. Reporting cost is
        // outside total_us and measured separately by the client receipt boundary.
        if let Ok(json) = serde_json::to_string(&report) { eprintln!("CUA_NATIVE_PROFILE {json}"); }
    }
}

/// Measure the existing operation without replacing it or changing its result.
pub struct Span { profile: Option<Arc<Profile>>, name: &'static str, start: Instant }
impl Span {
    pub fn new(name: &'static str) -> Self {
        Self { profile: ACTIVE.lock().unwrap().clone(), name, start: Instant::now() }
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        if let Some(profile) = &self.profile { profile.record(self.name, self.start, Instant::now()); }
    }
}
pub fn timed<T>(name: &'static str, work: impl FnOnce() -> T) -> T {
    let _span = Span::new(name);
    work()
}
