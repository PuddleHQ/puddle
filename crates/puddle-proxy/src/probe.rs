// SPDX-License-Identifier: GPL-3.0-or-later
//! The host's own probe of a TCP port, for telling a network that blocks a port from a Puddle
//! refusal (the Azure DevOps SSH message).

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use tokio::time::Instant;

use crate::destination::BoxFuture;

/// Whether this machine, on its own network, can open a TCP connection to a port.
pub trait PortProbe: Send + Sync {
    /// `true` when a connection to `host:port` opened; `false` when it was refused, timed out or
    /// the name did not resolve. Never errors: an unknown answer is `false`.
    fn reachable<'a>(&'a self, host: &'a str, port: u16) -> BoxFuture<'a, bool>;
}

/// [`PortProbe`] that opens a TCP connection and closes it, with a timeout, and remembers each
/// answer for a while so a client that retries does not make a probe each time.
#[derive(Debug)]
pub struct TcpPortProbe {
    timeout: Duration,
    remember: Duration,
    answers: Mutex<HashMap<(String, u16), (Instant, bool)>>,
}

impl TcpPortProbe {
    /// Waits `timeout` for a connection and keeps an answer for `remember`.
    #[must_use]
    pub fn new(timeout: Duration, remember: Duration) -> Self {
        Self {
            timeout,
            remember,
            answers: Mutex::new(HashMap::new()),
        }
    }

    fn remembered(&self, key: &(String, u16)) -> Option<bool> {
        let answers = self.answers.lock().unwrap_or_else(PoisonError::into_inner);
        answers
            .get(key)
            .filter(|(at, _)| at.elapsed() < self.remember)
            .map(|(_, answer)| *answer)
    }
}

impl Default for TcpPortProbe {
    /// Three seconds to connect, an answer kept for one minute.
    fn default() -> Self {
        Self::new(Duration::from_secs(3), Duration::from_mins(1))
    }
}

impl PortProbe for TcpPortProbe {
    fn reachable<'a>(&'a self, host: &'a str, port: u16) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let key = (host.to_owned(), port);
            if let Some(answer) = self.remembered(&key) {
                return answer;
            }
            let opened = matches!(
                tokio::time::timeout(self.timeout, tokio::net::TcpStream::connect((host, port)))
                    .await,
                Ok(Ok(_))
            );
            self.answers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key, (Instant::now(), opened));
            opened
        })
    }
}

#[cfg(test)]
mod tests;
