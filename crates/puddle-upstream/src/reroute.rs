// SPDX-License-Identifier: GPL-3.0-or-later
//! Telling a long-lived connection that the route it was made over is gone.
//!
//! A tunnel can stay open for hours. When the machine changes network (the VPN comes up, the
//! machine leaves the office) the proxy settings answer differently, new connections take the new
//! route at once, and a tunnel still on the old one would keep going to a proxy that is no longer
//! the way out, or around one that has become mandatory. A [`RouteWatch`] lets the holder of such a
//! tunnel find out and close it, so its owner reconnects over the new route.

use std::sync::Arc;

use tokio::sync::watch;

use crate::discovery::Discovery;
use crate::hop::{Destination, Hop};

/// Watches whether the hop a connection was made over is still part of the route to its
/// destination. Cheap: one receiver and the connection's destination.
#[derive(Debug)]
pub struct RouteWatch {
    discovery: Arc<Discovery>,
    epochs: watch::Receiver<u64>,
    destination: Destination,
    hop: Hop,
    /// The epoch the connection was made in.
    epoch: u64,
}

impl RouteWatch {
    pub(crate) fn new(
        discovery: &Arc<Discovery>,
        destination: Destination,
        hop: Hop,
        epoch: u64,
    ) -> Self {
        Self {
            // Subscribe before the first check, so an epoch that begins in between is seen.
            epochs: discovery.subscribe(),
            discovery: Arc::clone(discovery),
            destination,
            hop,
            epoch,
        }
    }

    /// Resolves once a new network epoch's route to the destination no longer contains the hop
    /// the connection was made over. A route that still lists the hop (as a fallback, say) keeps
    /// the connection: nothing is wrong with it. Never resolves while the epoch is unchanged.
    pub async fn changed(&mut self) {
        loop {
            let now = *self.epochs.borrow_and_update();
            if now != self.epoch {
                self.epoch = now;
                let decision = self.discovery.route(&self.destination).await;
                if !decision.route.hops().contains(&self.hop) {
                    tracing::info!(
                        destination = %self.destination.host(),
                        hop = %self.hop,
                        route = %decision.route,
                        "the network changed and the route no longer includes this connection's hop"
                    );
                    return;
                }
                continue;
            }
            if self.epochs.changed().await.is_err() {
                // The discovery is gone: nothing can change the route any more.
                std::future::pending::<()>().await;
            }
        }
    }
}
