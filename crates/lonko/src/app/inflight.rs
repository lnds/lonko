//! Drop-on-completion guard for `AtomicBool`-backed in-flight flags.
//!
//! `on_tick` schedules several recurring blocking tasks (active-pane
//! poll, tmux session refresh, tmux pane scan). Each one needs the
//! same invariant: at most one task in flight, and the flag must
//! reset on completion no matter how the task ends — including
//! panics. The pattern was previously inlined three times as a
//! local `struct Guard ...; impl Drop for Guard ...`. This module
//! collapses it into one place so the next caller can't omit the
//! `Drop` impl by accident.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// RAII handle on an in-flight flag. Constructing one transitions
/// the flag from `false` to `true` atomically; dropping resets it
/// to `false`. Returns `None` if the flag was already `true` (a
/// task is already running for that role).
pub(super) struct InflightGuard(Arc<AtomicBool>);

impl InflightGuard {
    pub(super) fn try_acquire(flag: &Arc<AtomicBool>) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| Self(flag.clone()))
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}
