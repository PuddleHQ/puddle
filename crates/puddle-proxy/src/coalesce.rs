// SPDX-License-Identifier: AGPL-3.0-or-later
//! One lookup in flight per name: connects that ask for a name while a lookup of it is running
//! share that lookup's answer instead of each starting their own.
//!
//! It is not a cache. The answer is handed to the callers that were waiting when it arrived and
//! forgotten, so no answer outlives the lookup it came from and a name that changes is seen by the
//! next connect. (The OS resolver reports no TTL, so a cache would have to guess one.) Rules are
//! decided before a name is resolved and the address check runs on every connect after it, so
//! sharing an answer between connects changes neither.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use puddle_types::DomainName;
use tokio::sync::OnceCell;

use crate::destination::{BoxFuture, Resolver};

/// The lookup's outcome as every waiter gets it: the error's kind and text, not its source chain.
type Answer = Result<Vec<SocketAddr>, (io::ErrorKind, String)>;
type Key = (DomainName, u16);

/// A lookup in flight and how many callers wait on it.
struct Flight {
    cell: Arc<OnceCell<Answer>>,
    waiters: usize,
}

type Flights = Mutex<HashMap<Key, Flight>>;

/// `inner`, with concurrent lookups of the same name and port shared. An answer is kept only
/// until its lookup returns; a user-supplied resolver is not wrapped.
pub struct Coalescing<R> {
    inner: R,
    flights: Flights,
}

impl<R: Resolver> Coalescing<R> {
    /// Shares `inner`'s concurrent lookups.
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            flights: Mutex::new(HashMap::new()),
        }
    }
}

fn lock(flights: &Flights) -> std::sync::MutexGuard<'_, HashMap<Key, Flight>> {
    flights.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Removes `key`'s flight if it is still `cell`'s (a later caller may have started a new one).
fn remove_if_ours(map: &mut HashMap<Key, Flight>, key: &Key, cell: &Arc<OnceCell<Answer>>) {
    if map.get(key).is_some_and(|f| Arc::ptr_eq(&f.cell, cell)) {
        map.remove(key);
    }
}

/// One waiter on a flight. The last waiter to leave takes the flight out of the map, so the map
/// never keeps a name whose every caller gave up (the connect's resolve timeout drops them).
struct Waiter<'a> {
    flights: &'a Flights,
    key: Key,
    cell: Arc<OnceCell<Answer>>,
}

impl<'a> Waiter<'a> {
    fn join(flights: &'a Flights, key: Key) -> Self {
        let mut map = lock(flights);
        let flight = map.entry(key.clone()).or_insert_with(|| Flight {
            cell: Arc::default(),
            waiters: 0,
        });
        flight.waiters += 1;
        let cell = Arc::clone(&flight.cell);
        drop(map);
        Self { flights, key, cell }
    }
}

impl Drop for Waiter<'_> {
    fn drop(&mut self) {
        let mut map = lock(self.flights);
        if let Some(flight) = map.get_mut(&self.key)
            && Arc::ptr_eq(&flight.cell, &self.cell)
        {
            flight.waiters -= 1;
            if flight.waiters == 0 {
                map.remove(&self.key);
            }
        }
    }
}

impl<R: Resolver> Resolver for Coalescing<R> {
    fn resolve<'a>(
        &'a self,
        name: &'a DomainName,
        port: u16,
    ) -> BoxFuture<'a, io::Result<Vec<SocketAddr>>> {
        Box::pin(async move {
            let waiter = Waiter::join(&self.flights, (name.clone(), port));
            let answer = waiter
                .cell
                .get_or_init(|| async {
                    let answer = self
                        .inner
                        .resolve(name, port)
                        .await
                        .map_err(|e| (e.kind(), e.to_string()));
                    // Out of the map the moment it has an answer, before the waiters wake, so a
                    // connect that comes after looks again.
                    remove_if_ours(&mut lock(&self.flights), &waiter.key, &waiter.cell);
                    answer
                })
                .await;
            match answer {
                Ok(addrs) => Ok(addrs.clone()),
                Err((kind, text)) => Err(io::Error::new(*kind, text.clone())),
            }
        })
    }
}

#[cfg(test)]
mod tests;
