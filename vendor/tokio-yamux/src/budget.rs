// SPDX-License-Identifier: MIT
//! puddle: the bytes of stream data one session holds on its way to the socket.

use std::{
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Waker},
};

/// What a session lets its streams queue before the socket takes it. A stream reserves bytes
/// before it sends a data frame; the session releases them once the socket write that carried
/// them completed. Without it each stream may queue its whole send window, so a peer that
/// grants windows and never reads makes the session hold streams x window bytes.
#[derive(Debug)]
pub(crate) struct SendBudget {
    limit: usize,
    used: AtomicUsize,
    ended: AtomicBool,
    waiting: Mutex<Vec<Waker>>,
}

impl SendBudget {
    pub(crate) fn new(limit: usize) -> SendBudget {
        SendBudget {
            limit,
            used: AtomicUsize::new(0),
            ended: AtomicBool::new(false),
            waiting: Mutex::new(Vec::new()),
        }
    }

    /// Reserves up to `want` bytes and returns how many it got. `None` parks the caller: its waker
    /// is woken when bytes are released or the session ends.
    pub(crate) fn reserve(&self, want: usize, cx: &Context) -> Option<usize> {
        // An ended session fails the send itself; parking would wait for a release that never comes.
        if want == 0 || self.ended.load(Ordering::Acquire) {
            return Some(want);
        }
        // A second try after parking closes the race with a release between the two.
        for attempt in 0..2 {
            let mut used = self.used.load(Ordering::Acquire);
            loop {
                let free = self.limit.saturating_sub(used);
                if free == 0 {
                    break;
                }
                let n = free.min(want);
                match self.used.compare_exchange_weak(
                    used,
                    used + n,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return Some(n),
                    Err(now) => used = now,
                }
            }
            if attempt == 0 {
                let mut waiting = self
                    .waiting
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if !waiting.iter().any(|w| w.will_wake(cx.waker())) {
                    waiting.push(cx.waker().clone());
                }
            }
        }
        None
    }

    /// Gives back bytes the socket took (or that were dropped unsent) and wakes the parked streams.
    pub(crate) fn release(&self, bytes: usize) {
        if bytes == 0 {
            return;
        }
        self.used.fetch_sub(bytes, Ordering::AcqRel);
        self.wake_all();
    }

    /// The session ended: nothing will be released, so every parked stream must look again.
    pub(crate) fn end(&self) {
        self.ended.store(true, Ordering::Release);
        self.wake_all();
    }

    fn wake_all(&self) {
        let parked = std::mem::take(
            &mut *self
                .waiting
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for waker in parked {
            waker.wake();
        }
    }
}
