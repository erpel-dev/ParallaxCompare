//! Progress and cancellation of long operations (copy, delete, archive rewrites),
//! shared between the worker and the caller.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::{Error, Result};

#[derive(Debug, Default)]
pub struct Progress {
    pub cancel: AtomicBool,
    /// Units done and expected: files for a copy, entries for an archive rewrite.
    pub done: AtomicU64,
    pub total: AtomicU64,
}

impl Progress {
    /// Fails with [`Error::Cancelled`] once the caller cancelled.
    pub fn check(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) { Err(Error::Cancelled) } else { Ok(()) }
    }

    pub fn expect(&self, units: u64) {
        self.total.fetch_add(units, Ordering::Relaxed);
    }

    /// Counts `units` as done and checks for cancellation.
    pub fn advance(&self, units: u64) -> Result<()> {
        self.done.fetch_add(units, Ordering::Relaxed);
        self.check()
    }
}
