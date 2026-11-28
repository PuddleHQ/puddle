// SPDX-License-Identifier: AGPL-3.0-or-later
//! The proxy's own threads for store calls (decisions, connection records).
//!
//! The app's runtime has one blocking pool that every crate draws from (vault, backups, restore,
//! API calls). A decision that waited in that queue would stall a guest's connection behind work
//! that has nothing to do with it, and a flood of decisions would take the threads the API needs.
//! A [`Lane`] owns a fixed number of threads instead: store calls queue only behind each other,
//! and the ambient pool's size is nobody's concern here.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::oneshot;

type Job = Box<dyn FnOnce() + Send>;

/// How many threads a lane runs. The store serializes its writes on one connection, so more
/// threads only let reads and rule lookups pass a waiting write.
const THREADS: usize = 4;

/// A fixed set of threads that run closures, for calls that may block on SQLite.
pub(crate) struct Lane {
    jobs: Sender<Job>,
}

/// The job panicked, so its result is lost.
#[derive(Debug)]
pub(crate) struct Panicked;

impl Lane {
    pub(crate) fn new() -> Self {
        let (jobs, queue) = channel::<Job>();
        let queue = Arc::new(Mutex::new(queue));
        for n in 0..THREADS {
            let queue = Arc::clone(&queue);
            let spawned = std::thread::Builder::new()
                .name(format!("proxy-store-{n}"))
                .spawn(move || work(&queue));
            if let Err(err) = spawned {
                tracing::warn!(%err, "a store thread of the proxy did not start");
            }
        }
        Self { jobs }
    }

    /// Runs `job` on a lane thread and returns its result.
    pub(crate) async fn run<T: Send + 'static>(
        &self,
        job: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, Panicked> {
        let (answer, answered) = oneshot::channel();
        self.detach(move || {
            drop(answer.send(job()));
        });
        answered.await.map_err(|_| Panicked)
    }

    /// Queues `job` and returns at once. If no lane thread is left to take it, it runs here.
    pub(crate) fn detach(&self, job: impl FnOnce() + Send + 'static) {
        if let Err(refused) = self.jobs.send(Box::new(job)) {
            (refused.0)();
        }
    }
}

fn work(queue: &Mutex<Receiver<Job>>) {
    loop {
        let next = queue.lock().unwrap_or_else(PoisonError::into_inner).recv();
        let Ok(job) = next else { return };
        // A panicking job drops its answer sender, which its caller reads as `Panicked`.
        drop(catch_unwind(AssertUnwindSafe(job)));
    }
}

#[cfg(test)]
mod tests;
