//! The UI's clock: animations, the live chart's refresh, the countdown and the banner all
//! read time from here.
//!
//! Normally this is just the system clock. When recording a video (feature `snapshot`),
//! the recorder stops the clock while it captures each frame, and the scan pauses with it,
//! so the recording plays back at the app's real speed however long capturing takes.

use std::time::{Duration, Instant};

pub fn now() -> Instant {
    #[cfg(feature = "snapshot")]
    return recording::now();
    #[cfg(not(feature = "snapshot"))]
    Instant::now()
}

pub fn since(earlier: Instant) -> Duration {
    now().saturating_duration_since(earlier)
}

/// The scan calls this before reading each folder; it waits while the clock is stopped.
#[inline]
pub fn gate() {
    #[cfg(feature = "snapshot")]
    recording::gate();
}

#[cfg(feature = "snapshot")]
pub mod recording {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Condvar, Mutex};
    use std::time::{Duration, Instant};

    struct State {
        /// Total time spent stopped, which the clock leaves out.
        stopped_total: Duration,
        stopped_at: Option<Instant>,
    }

    static STOPPED: AtomicBool = AtomicBool::new(false);
    static STATE: Mutex<State> = Mutex::new(State { stopped_total: Duration::ZERO, stopped_at: None });
    static RESUMED: Condvar = Condvar::new();

    pub fn now() -> Instant {
        let state = STATE.lock().unwrap();
        let real = state.stopped_at.unwrap_or_else(Instant::now);
        real - state.stopped_total
    }

    /// Freeze time and the scan (folders already being read finish first).
    pub fn stop() {
        let mut state = STATE.lock().unwrap();
        if state.stopped_at.is_none() {
            state.stopped_at = Some(Instant::now());
            STOPPED.store(true, Ordering::Release);
        }
    }

    pub fn start() {
        let mut state = STATE.lock().unwrap();
        if let Some(at) = state.stopped_at.take() {
            state.stopped_total += at.elapsed();
            STOPPED.store(false, Ordering::Release);
            RESUMED.notify_all();
        }
    }

    pub fn gate() {
        if !STOPPED.load(Ordering::Acquire) {
            return;
        }
        let mut state = STATE.lock().unwrap();
        while state.stopped_at.is_some() {
            state = RESUMED.wait(state).unwrap();
        }
    }
}
