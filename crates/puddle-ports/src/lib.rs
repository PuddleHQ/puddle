// SPDX-License-Identifier: GPL-3.0-or-later
//! The ports of a workspace: what listens in the guest, forwards from the guest to this computer's
//! loopback, and pinned forwards that come back at every start.
//!
//! ```text
//!  browser ──► 127.0.0.1:3000 ──► Forward ──► GuestLink::open_forward(3000) ──► agent ──► 127.0.0.1:3000 in the guest
//!  (this computer)  (registered as one of Puddle's own listeners)  (yamux stream, no SSH)
//! ```
//!
//! | Piece | What |
//! |---|---|
//! | [`Ports`] | the state of every workspace's ports; the host calls it when a workspace starts, stops or is deleted, and the proxy's route feeds it what the agent reports ([`puddle_agent_proto::host::GuestWatcher`]) |
//! | [`Pins`] | where pinned forwards are kept (the store) |
//! | [`PortOwner`], [`ProgramLookup`] | who holds a port of this computer, for the message that says a forward cannot use it |
//!
//! A forward binds `127.0.0.1` only. Its listener is in the registry of Puddle's own listeners
//! ([`puddle_netpolicy::PuddleEndpoints`]), so no workspace can reach it from the inside: not its
//! own forward, not another workspace's. A forward does not use the SSH endpoint, so an editor's
//! forwards and these never count against each other (the runtime's SSH server caps forwards per
//! connection), and turning direct SSH off does not touch them.
#![forbid(unsafe_code)]

mod forward;
mod owner;
mod pins;
mod ports;

pub use forward::MAX_CONNECTIONS;
pub use owner::{PortOwner, ProgramLookup, SystemPrograms};
pub use pins::{MemoryPins, Pin, Pins, PinsError};
pub use ports::{NoLink, PortRow, Ports, PortsError, PortsView};
