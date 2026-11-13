// SPDX-License-Identifier: AGPL-3.0-or-later
//! The fake runtime's SSH server with a pseudo-terminal stand-in, for tests of a client that asks
//! for a shell ([`FakeRuntime::serve_terminals`](super::FakeRuntime::serve_terminals)).
//!
//! It is a real SSH server (russh) that accepts any key, so a test goes through the same
//! handshake, pty request, shell request and window changes as against a real guest. The "shell"
//! echoes what it is typed and understands a few lines:
//!
//! | Line | Answer |
//! |---|---|
//! | `exit N` | the exit status `N`, then the channel closes |
//! | `size` | `<rows> <cols>` of the last pty request or window change |
//! | `whoami` | the user the client logged in as |
//! | `term` | the `TERM` the pty was asked for |
//! | `command` | the command the shell was started with (an exec request), empty for a plain shell request |
//! | `flood` | output without end, until the connection closes (a program like `yes`) |
//! | anything else | nothing, then a new prompt |
//!
//! A Ctrl+C byte ends a `flood`. A few login names make the server misbehave, for a client's
//! failure paths: `denied` is refused at login, `nopty` and `noshell` get no pseudo-terminal or
//! shell, `hangup` has the channel closed at the pty request, `chatty` sends a stray message before
//! it answers the pty request, and `kill` as a line ends the shell
//! by a signal.

use std::sync::Arc;

use russh::server::{Auth, Config, Handler, Session, VerifiedPublicKey};
use russh::server::{ChannelOpenHandle, Msg};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use tokio::task::JoinHandle;

use crate::SshStream;

/// The prompt of the stand-in shell.
pub const PROMPT: &str = "$ ";

struct Shell {
    user: String,
    term: String,
    cols: u32,
    rows: u32,
    started_with: String,
    line: Vec<u8>,
    flood: Option<JoinHandle<()>>,
}

impl Drop for Shell {
    fn drop(&mut self) {
        if let Some(flood) = self.flood.take() {
            flood.abort();
        }
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "the trait's methods are async; the stand-in answers at once"
)]
impl Handler for Shell {
    type Error = russh::Error;

    async fn auth_publickey(
        &mut self,
        user: &str,
        _key: VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        user.clone_into(&mut self.user);
        if user == "denied" {
            return Ok(Auth::Reject {
                proceed_with_methods: Some(MethodSet::from(&[MethodKind::PublicKey][..])),
                partial_success: false,
            });
        }
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        match self.user.as_str() {
            "nopty" => return session.channel_failure(channel),
            "hangup" => return session.close(channel),
            // A stray message ahead of the answer, as a real server sends window adjustments.
            "chatty" => session.data(channel, b"banner".to_vec())?,
            _ => {}
        }
        term.clone_into(&mut self.term);
        self.cols = col_width;
        self.rows = row_height;
        session.channel_success(channel)
    }

    async fn env_request(
        &mut self,
        channel: ChannelId,
        _name: &str,
        _value: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.start(channel, "", session)
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.start(channel, &String::from_utf8_lossy(data), session)
    }

    async fn window_change_request(
        &mut self,
        _channel: ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.cols = col_width;
        self.rows = row_height;
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        for &byte in data {
            match byte {
                3 => {
                    if let Some(flood) = self.flood.take() {
                        flood.abort();
                    }
                    self.line.clear();
                    session.data(channel, format!("^C\r\n{PROMPT}").into_bytes())?;
                }
                b'\r' | b'\n' => {
                    let line =
                        String::from_utf8_lossy(&std::mem::take(&mut self.line)).into_owned();
                    session.data(channel, b"\r\n".to_vec())?;
                    if !self.run(channel, line.trim(), session)? {
                        return Ok(());
                    }
                }
                _ => {
                    self.line.push(byte);
                    session.data(channel, vec![byte])?;
                }
            }
        }
        Ok(())
    }
}

impl Shell {
    /// Starts the shell, as a shell request (`command` empty) or an exec request.
    fn start(
        &mut self,
        channel: ChannelId,
        command: &str,
        session: &mut Session,
    ) -> Result<(), russh::Error> {
        if self.user == "noshell" {
            return session.channel_failure(channel);
        }
        command.clone_into(&mut self.started_with);
        session.channel_success(channel)?;
        session.data(channel, PROMPT.as_bytes().to_vec())
    }

    /// Runs one line; `false` when the shell ended.
    fn run(
        &mut self,
        channel: ChannelId,
        line: &str,
        session: &mut Session,
    ) -> Result<bool, russh::Error> {
        let answer = match line.split_once(' ').unwrap_or((line, "")) {
            ("exit", status) => {
                let status = status.trim().parse().unwrap_or(0);
                session.exit_status_request(channel, status)?;
                session.eof(channel)?;
                session.close(channel)?;
                return Ok(false);
            }
            ("kill", _) => {
                session.exit_signal_request(channel, russh::Sig::KILL, false, "", "")?;
                session.eof(channel)?;
                session.close(channel)?;
                return Ok(false);
            }
            ("size", _) => format!("{} {}\r\n", self.rows, self.cols),
            ("whoami", _) => format!("{}\r\n", self.user),
            ("term", _) => format!("{}\r\n", self.term),
            ("command", _) => format!("{}\r\n", self.started_with),
            ("flood", _) => {
                let handle = session.handle();
                self.flood = Some(tokio::spawn(async move {
                    let chunk = vec![b'y'; 32 * 1024];
                    // Ends only when the shell is dropped, which aborts this task.
                    loop {
                        let _ = handle.data(channel, chunk.clone()).await;
                        tokio::task::yield_now().await;
                    }
                }));
                return Ok(true);
            }
            _ => String::new(),
        };
        session.data(channel, format!("{answer}{PROMPT}").into_bytes())?;
        Ok(true)
    }
}

/// Serves one SSH connection on `stream` with the stand-in shell, until it ends.
///
/// # Errors
///
/// Whatever ended the connection abnormally, as text.
pub async fn serve<S: SshStream>(stream: S) -> Result<(), String> {
    let key = russh::keys::PrivateKey::from(
        russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[7; 32]),
    );
    let config = Config {
        keys: vec![key],
        methods: MethodSet::from(&[MethodKind::PublicKey][..]),
        inactivity_timeout: None,
        auth_rejection_time: std::time::Duration::ZERO,
        ..Config::default()
    };
    let shell = Shell {
        user: String::new(),
        term: String::new(),
        cols: 0,
        rows: 0,
        started_with: String::new(),
        line: Vec::new(),
        flood: None,
    };
    let session = russh::server::run_stream(Arc::new(config), stream, shell)
        .await
        .map_err(|e| e.to_string())?;
    session.await.map_err(|e| e.to_string())
}
