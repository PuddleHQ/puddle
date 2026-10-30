// SPDX-License-Identifier: GPL-3.0-or-later
//! The browser editor's front: one listener on the user's loopback that carries HTTP and
//! `WebSockets` from `<workspace>.localhost:<port>` to the editor server in that workspace's guest.
//!
//! ```text
//!  window ──► <workspace>.localhost:<port> ──► Front ──► GuestLink::open_editor() ──► agent ──► 127.0.0.1:41337 in the guest
//!             (one listener, routed by Host)   (yamux stream, no SSH)
//! ```
//!
//! - **One origin per workspace.** Cookies and browser storage are scoped by host name, so the name
//!   keeps one workspace's editor away from another's. A request whose `Host` is not
//!   exactly `<workspace>.localhost:<this port>` is refused ([`host_header`]), as is one whose
//!   `Origin` is another origin. The desktop shell's names (`tauri`, `ipc`, `asset`) are not
//!   workspace names, so no origin collides with the shell's.
//! - **One origin per forwarded port.** `<port>.<workspace>.localhost:<this port>` carries HTTP and
//!   `WebSockets` to that port on the guest's loopback ([`Editor::connect_port`], the stream the
//!   Ports tab's forwards use, so the agent's own ports stay refused). It is the editor's
//!   `/proxy/<port>/` links' target; the editor's session and Microsoft's key cookie are scoped to
//!   the editor's own host name and never reach it, and no frame header is added to the app's answers.
//! - **Microsoft's server** loses a thing its launcher does for it: persistent secret storage.
//!   The front gives it back ([`microsoft`], [`keys`]).
//! - **Frame headers** (`frame-ancestors 'self'`, `X-Frame-Options: SAMEORIGIN`) are added to every
//!   answer.
//! - **A Puddle listener.** The front is entered in the registry of Puddle's own listeners, so no
//!   workspace can reach it from the inside, whatever its toggles and rules
//!   ([`puddle_netpolicy::PuddleEndpoints`]). It binds `127.0.0.1` only.
//! - **Connected windows** are counted per workspace ([`Windows`]), for waiting before the server
//!   is replaced.
//!
//! The front knows workspaces only through [`Guests`], and asks on every request. It holds no
//! credential: the editor server's own password check stays in front of the guest, and the
//! password is read through [`Front::password`] for the window to sign in with.
#![forbid(unsafe_code)]

pub mod editor;
pub mod host_header;
pub mod keys;
pub mod microsoft;
mod proxy;
mod server;
pub mod windows;

pub use editor::{Duplex, Editor, Flavour, Guests, Io, PortsGuests, Target, Unreachable};
pub use keys::{FileHalves, ServerHalves};
pub use server::{Front, MAX_CONNECTIONS, origin_of};
pub use windows::Windows;
