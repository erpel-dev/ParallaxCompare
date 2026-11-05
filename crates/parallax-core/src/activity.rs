//! What slow work is going on right now, such as reading an archive's index or
//! decompressing a 7z block, for a progress line to name it.
//!
//! Work registers itself with [`begin`] and is done when the returned guard is
//! dropped. Short work never shows: [`current`] reports only what has been going
//! on for a moment.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

static NEXT: AtomicU64 = AtomicU64::new(0);
static RUNNING: Mutex<Vec<(u64, Instant, String)>> = Mutex::new(Vec::new());

/// Work shorter than this is not reported.
const NOTICEABLE: Duration = Duration::from_millis(300);

/// Ends the activity when dropped.
pub struct Activity(u64);

impl Drop for Activity {
    fn drop(&mut self) {
        RUNNING.lock().retain(|(id, ..)| *id != self.0);
    }
}

/// Registers `what` ("Decompressing release.7z") until the guard is dropped.
pub fn begin(what: impl Into<String>) -> Activity {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    RUNNING.lock().push((id, Instant::now(), what.into()));
    Activity(id)
}

/// The longest-running activity that has gone on for a moment, if any.
pub fn current() -> Option<String> {
    let running = RUNNING.lock();
    running
        .iter()
        .filter(|(_, since, _)| since.elapsed() >= NOTICEABLE)
        .min_by_key(|(_, since, _)| *since)
        .map(|(.., what)| what.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_lasting_work_is_reported() {
        let quick = begin("quick work");
        assert_eq!(current(), None);
        drop(quick);
        let slow = begin("Decompressing a.7z");
        std::thread::sleep(NOTICEABLE + Duration::from_millis(50));
        assert_eq!(current().as_deref(), Some("Decompressing a.7z"));
        drop(slow);
        assert_eq!(current(), None);
    }
}
