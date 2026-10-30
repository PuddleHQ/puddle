// SPDX-License-Identifier: GPL-3.0-or-later
//! What the front needs of a workspace: a way to reach its editor server, and which server it is.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use puddle_agent_proto::forward::{GuestLink, OpenError};
use puddle_ports::Ports;
use puddle_types::WorkspaceName;
use tokio::io::{AsyncRead, AsyncWrite};

/// A byte stream to the editor server.
pub trait Duplex: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> Duplex for T {}

/// A connection to the editor server, whatever carries it.
pub type Io = Box<dyn Duplex>;

/// A future that is boxed so the traits below can be used as `dyn`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Why the editor server could not be reached. The text is for the person looking at the window.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Unreachable(pub String);

/// The way to one workspace's editor server.
pub trait Editor: Send + Sync {
    /// Opens a connection to the server.
    fn connect(&self) -> BoxFuture<'_, Result<Io, Unreachable>>;

    /// Opens a connection to `port` on the guest's loopback: Puddle's forward of that port. The
    /// way in is the one the Ports tab's forwards use, so the agent's own ports are refused there.
    fn connect_port(&self, port: u16) -> BoxFuture<'_, Result<Io, Unreachable>>;

    /// The server's password for this boot of the sandbox.
    fn password(&self) -> BoxFuture<'_, Result<String, Unreachable>>;
}

impl From<OpenError> for Unreachable {
    fn from(err: OpenError) -> Self {
        Self(err.to_string())
    }
}

/// The agent's `editor` stream is the way in ([`puddle_agent_proto::editor_stream`]).
impl Editor for GuestLink {
    fn connect(&self) -> BoxFuture<'_, Result<Io, Unreachable>> {
        Box::pin(async move { Ok(Box::new(self.open_editor().await?) as Io) })
    }

    fn connect_port(&self, port: u16) -> BoxFuture<'_, Result<Io, Unreachable>> {
        Box::pin(async move { Ok(Box::new(self.open_forward(port).await?) as Io) })
    }

    fn password(&self) -> BoxFuture<'_, Result<String, Unreachable>> {
        Box::pin(async move { Ok(self.editor_password().await?) })
    }
}

/// Which editor server runs in the guest. The front treats them alike except for what Microsoft's
/// server's own launcher normally does for it ([`crate::microsoft`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Flavour {
    /// code-server, the bundled default.
    CodeServer,
    /// Microsoft's VS Code server, started directly.
    Microsoft,
}

/// A running workspace's editor, as the front finds it for a request.
#[derive(Clone)]
pub struct Target {
    /// The way to the server.
    pub editor: Arc<dyn Editor>,
    /// Which server it is.
    pub flavour: Flavour,
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("flavour", &self.flavour)
            .finish_non_exhaustive()
    }
}

/// The workspaces the front can serve. It asks on every request, so a workspace that stops is
/// refused at once and one that starts is served at once.
pub trait Guests: Send + Sync {
    /// The editor of `workspace`, `None` while the workspace is not running.
    fn target(&self, workspace: &WorkspaceName) -> Option<Target>;
}

/// The workspaces [`Ports`] holds an agent link for: a workspace is served while its agent is
/// connected. Which server runs there is the caller's to say.
pub struct PortsGuests {
    ports: Ports,
    flavour: Box<dyn Fn(&WorkspaceName) -> Flavour + Send + Sync>,
}

impl PortsGuests {
    /// Serves the workspaces of `ports`, each with the server `flavour` names.
    #[must_use]
    pub fn new(
        ports: Ports,
        flavour: impl Fn(&WorkspaceName) -> Flavour + Send + Sync + 'static,
    ) -> Self {
        Self {
            ports,
            flavour: Box::new(flavour),
        }
    }
}

impl Guests for PortsGuests {
    fn target(&self, workspace: &WorkspaceName) -> Option<Target> {
        let link = self.ports.guest_link(workspace)?;
        Some(Target {
            editor: Arc::new(link),
            flavour: (self.flavour)(workspace),
        })
    }
}
