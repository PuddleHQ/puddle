// SPDX-License-Identifier: AGPL-3.0-or-later
//! One SSH connection from a workspace to a terminated Git host: the server, its channels, the
//! table check and the audit record.
#![expect(
    clippy::unused_async_trait_impl,
    reason = "the methods of russh's `Handler` are async whether or not they wait"
)]

use std::collections::{HashMap, HashSet};
use std::pin::pin;
use std::sync::{Arc, Mutex, PoisonError};

use puddle_types::{
    ConnectionDecision, ConnectionEvent, ConnectionReason, EgressRequest, HttpRequestLine,
    WorkspaceId,
};
use russh::server::{Auth, Msg, Session};
use russh::{Channel, ChannelId, ChannelOpenFailure};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep_until, timeout_at};

use super::grammar::{self, Command, GitHost, Refused, Repository};
use super::{GitSsh, GitSshCommand, GitSshContext, GitSshLimits, GitSshUpstream};
use crate::proxy::{ClientReader, WorkspaceHandler, admit, apply_reason, refuse};
use crate::terminate::{
    InjectContext, InjectDecision, InjectRefusal, Injector, MAX_UNSWAPPED_STAND_INS, Outcome,
    Prefixed, RequestView, Termination,
};

/// The audit code of a command that is none of the grammar's.
const UNSUPPORTED: &str = "ssh_command_unsupported";
/// The audit code of a channel request that is refused.
const CHANNEL_REFUSED: &str = "ssh_channel_refused";
/// The audit code of a path that is not one Puddle reads.
const BAD_PATH: &str = "bad_git_path";

/// What the connection's record is made of, written by the channels as they go.
#[derive(Default)]
struct Summary {
    first: Option<HttpRequestLine>,
    refused: Option<(HttpRequestLine, &'static str)>,
    injected: bool,
    binding_id: Option<String>,
}

impl Summary {
    fn command(&mut self, line: HttpRequestLine) {
        if self.first.is_none() {
            self.first = Some(line);
        }
    }

    fn refusal(&mut self, line: HttpRequestLine, code: &'static str) {
        if self.refused.is_none() {
            self.refused = Some((line, code));
        }
    }

    fn credential(&mut self, binding: Option<&str>) {
        if let Some(id) = binding
            && self.binding_id.is_none()
        {
            self.injected = true;
            self.binding_id = Some(id.to_owned());
        }
    }
}

/// What the connection shares with its channels' tasks.
struct Ctx {
    workspace: WorkspaceId,
    host: GitHost,
    /// What the upstream is given about the connection.
    route: GitSshContext,
    injector: Arc<dyn Injector>,
    upstream: Arc<dyn GitSshUpstream>,
    summary: Mutex<Summary>,
    open: Open,
}

impl Ctx {
    fn summary(&self) -> std::sync::MutexGuard<'_, Summary> {
        self.summary.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// How far a connection is, which sets the timer that watches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Before authentication: the handshake limit runs.
    Handshake,
    /// Authenticated, no channel open: the idle limit runs.
    Idle,
    /// At least one channel open.
    Busy,
}

/// The open session channels and the phase they put the connection in.
struct Open {
    channels: Mutex<HashSet<ChannelId>>,
    phase: watch::Sender<Phase>,
}

impl Open {
    fn count(&self) -> usize {
        self.channels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    fn add(&self, id: ChannelId) {
        self.channels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id);
        self.update();
    }

    fn remove(&self, id: ChannelId) {
        self.channels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
        self.update();
    }

    /// The client authenticated: from here the idle limit watches the connection.
    fn authenticated(&self) {
        self.update();
    }

    fn update(&self) {
        let phase = if self.count() == 0 {
            Phase::Idle
        } else {
            Phase::Busy
        };
        self.phase.send_if_modified(|current| {
            let changed = *current != phase;
            *current = phase;
            changed
        });
    }
}

/// Serves `request`'s connection as Puddle's SSH server and returns what the audit records.
pub(crate) async fn serve<S>(
    handler: &WorkspaceHandler,
    mut reader: ClientReader<S>,
    request: &EgressRequest,
    git: &GitSsh,
    termination: &Termination,
    host: GitHost,
) -> ConnectionEvent
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    // The rules decide an SSH connection as they decide an HTTPS one to the same name (they
    // ignore the port): a name the workspace may not reach is not served.
    let https = EgressRequest::new(request.workspace.clone(), request.host.clone(), 443);
    let (admitted, mut event) = admit(&handler.proxy, &https).await;
    event.port = request.port;
    if let Err(refusal) = admitted {
        event.outcome = refusal.outcome;
        refuse(reader.get_mut(), &refusal).await;
        return event;
    }
    let early = reader.buffer().to_vec();
    let mut stream = reader.into_inner();
    // A guest that is gone fails the handshake below, which ends the connection the same way.
    let _ = stream
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await;
    // libssh2 (libgit2, so cargo's built-in Git) treats an accepted `none` as a failed login.
    let libssh2 = names_libssh2(&early);
    let stream = Prefixed::new(stream, early);
    let (phase, mut watching) = watch::channel(Phase::Handshake);
    let ctx = Arc::new(Ctx {
        workspace: request.workspace.clone(),
        host,
        route: GitSshContext::new(handler.clone()),
        injector: Arc::clone(termination.injector()),
        upstream: Arc::clone(&git.upstream),
        summary: Mutex::new(Summary::default()),
        open: Open {
            channels: Mutex::new(HashSet::new()),
            phase,
        },
    });
    let config = Arc::new(server_config(
        termination
            .git_ssh_keys()
            .map_or_else(|| git.host_keys.as_ref().clone(), |own| own.keys().to_vec()),
    ));
    let conn = Conn {
        ctx: Arc::clone(&ctx),
        libssh2,
        slots: HashMap::new(),
        tasks: JoinSet::new(),
        limits: git.limits,
    };
    watch_connection(&ctx, config, stream, conn, &mut watching, git.limits).await;
    let summary = ctx.summary();
    event.http = summary
        .refused
        .as_ref()
        .map(|(line, _)| line.clone())
        .or_else(|| summary.first.clone());
    event.injected = summary.injected;
    event.binding_id = summary.binding_id.clone();
    apply_legs(&mut event, &ctx.route.legs());
    // A refusal Puddle's own server gave is the connection's reason, over a rule's refusal
    // inside an HTTPS leg.
    if let Some((_, code)) = &summary.refused {
        event.decision = ConnectionDecision::Blocked;
        event.reason = ConnectionReason::Refused(code);
    }
    event
}

/// Puts what the HTTPS legs of the commands saw on the connection's record, as a decrypted
/// connection's own end does: the first leg's address, hop and connection outcome, the first
/// refusal by a rule on method and path, the credentials and stand-ins of every leg. The legs'
/// request lines stay off the record: it names the SSH command the guest sent.
fn apply_legs(event: &mut ConnectionEvent, legs: &[Outcome]) {
    let Some(first) = legs.first() else {
        return;
    };
    event.resolved_ip = first.resolved_ip;
    event.upstream.clone_from(&first.hop);
    event.outcome = first.connect;
    apply_reason(
        event,
        legs.iter()
            .find(|l| l.refused_by_rule.is_some())
            .unwrap_or(first),
    );
    for leg in legs {
        if leg.injected && !event.injected {
            event.injected = true;
            event.binding_id.clone_from(&leg.binding_id);
        }
        for (all, more) in [
            (&mut event.unswapped_stand_ins, &leg.unswapped_stand_ins),
            (&mut event.scrubbed_stand_ins, &leg.scrubbed_stand_ins),
            (&mut event.foreign_credentials, &leg.foreign_credentials),
            (&mut event.answer_credentials, &leg.answer_credentials),
        ] {
            for id in more {
                if all.len() < MAX_UNSWAPPED_STAND_INS && !all.contains(id) {
                    all.push(id.clone());
                }
            }
        }
    }
}

/// Runs the SSH session and closes it when a limit passes.
async fn watch_connection<S>(
    ctx: &Ctx,
    config: Arc<russh::server::Config>,
    stream: S,
    conn: Conn,
    watching: &mut watch::Receiver<Phase>,
    limits: GitSshLimits,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let handshake_ends = Instant::now() + limits.handshake;
    let started = match timeout_at(
        handshake_ends,
        russh::server::run_stream(config, stream, conn),
    )
    .await
    {
        Ok(Ok(running)) => running,
        Ok(Err(err)) => {
            tracing::debug!(error = %err, "ssh handshake failed");
            return;
        }
        Err(_) => {
            tracing::info!(workspace = %ctx.workspace, "ssh handshake took too long");
            return;
        }
    };
    // A russh session runs in a task of its own that outlives its handle: end it however this
    // function does.
    let _disconnect = Disconnect(started.handle());
    let mut running = pin!(started);
    loop {
        let phase = *watching.borrow_and_update();
        let limit = match phase {
            Phase::Handshake => Some(handshake_ends),
            Phase::Idle => Some(Instant::now() + limits.idle),
            Phase::Busy => None,
        };
        tokio::select! {
            _ = &mut running => return,
            // The sender lives as long as the connection's context: a change only restarts the timer.
            _ = watching.changed() => {}
            () = async {
                match limit {
                    Some(at) => sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => {
                tracing::info!(workspace = %ctx.workspace, ?phase, "ssh connection closed at its limit");
                return;
            }
        }
    }
}

/// Disconnects a session when dropped, so a limit or a cancelled connection ends it.
struct Disconnect(russh::server::Handle);

impl Drop for Disconnect {
    fn drop(&mut self) {
        tokio::spawn(disconnect(self.0.clone()));
    }
}

async fn disconnect(handle: russh::server::Handle) {
    // the guest may have disconnected first; the session is over either way
    let _ = handle
        .disconnect(
            russh::Disconnect::ByApplication,
            "closed by Puddle".to_owned(),
            String::new(),
        )
        .await;
}

/// A session channel the client opened and has not used yet.
struct Slot {
    channel: Option<Channel<Msg>>,
    /// The `GIT_PROTOCOL` versions the guest named, in order.
    versions: Vec<u8>,
    /// A command or a shell was started on it.
    started: bool,
}

/// The SSH server's handler for one connection.
struct Conn {
    ctx: Arc<Ctx>,
    libssh2: bool,
    slots: HashMap<ChannelId, Slot>,
    /// The channels' tasks; dropped with the connection, which ends them.
    tasks: JoinSet<()>,
    limits: GitSshLimits,
}

impl Conn {
    fn refuse_channel(&self, what: &str) {
        tracing::debug!(what, "ssh channel request refused");
        self.ctx
            .summary()
            .refusal(HttpRequestLine::new("ssh-channel", "/"), CHANNEL_REFUSED);
    }
}

impl russh::server::Handler for Conn {
    type Error = russh::Error;

    async fn auth_none(&mut self, _: &str) -> Result<Auth, Self::Error> {
        if self.libssh2 {
            return Ok(Auth::Reject {
                proceed_with_methods: Some(russh::MethodSet::from(
                    &[russh::MethodKind::PublicKey, russh::MethodKind::Password][..],
                )),
                partial_success: false,
            });
        }
        Ok(Auth::Accept)
    }

    async fn auth_password(&mut self, _: &str, _: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn auth_publickey_offered(
        &mut self,
        _: &str,
        _: russh::server::PublicKeyOffer<'_>,
    ) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn auth_publickey(
        &mut self,
        _: &str,
        _: russh::server::VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        _: &str,
        _: &str,
        _: Option<russh::server::Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn auth_succeeded(&mut self, _: &mut Session) -> Result<(), Self::Error> {
        self.ctx.open.authenticated();
        Ok(())
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.ctx.open.count() >= self.limits.channels {
            self.refuse_channel("too many channels");
            reply.reject(ChannelOpenFailure::ResourceShortage).await;
            return Ok(());
        }
        let id = channel.id();
        self.ctx.open.add(id);
        self.slots.insert(
            id,
            Slot {
                channel: Some(channel),
                versions: Vec::new(),
                started: false,
            },
        );
        reply.accept().await;
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.slots.remove(&channel);
        self.ctx.open.remove(channel);
        Ok(())
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        _: Channel<Msg>,
        _: &str,
        _: u32,
        _: &str,
        _: u32,
        reply: russh::server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.refuse_channel("port forwarding");
        reply.reject(prohibited("port forwarding")).await;
        Ok(())
    }

    async fn channel_open_direct_streamlocal(
        &mut self,
        _: Channel<Msg>,
        _: &str,
        reply: russh::server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.refuse_channel("socket forwarding");
        reply.reject(prohibited("socket forwarding")).await;
        Ok(())
    }

    async fn channel_open_x11(
        &mut self,
        _: Channel<Msg>,
        _: &str,
        _: u32,
        reply: russh::server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.refuse_channel("x11");
        reply.reject(prohibited("X11 forwarding")).await;
        Ok(())
    }

    async fn tcpip_forward(
        &mut self,
        _: &str,
        _: &mut u32,
        _: &mut Session,
    ) -> Result<bool, Self::Error> {
        self.refuse_channel("remote forwarding");
        Ok(false)
    }

    async fn streamlocal_forward(&mut self, _: &str, _: &mut Session) -> Result<bool, Self::Error> {
        self.refuse_channel("remote socket forwarding");
        Ok(false)
    }

    async fn agent_request(&mut self, _: ChannelId, _: &mut Session) -> Result<bool, Self::Error> {
        self.refuse_channel("agent forwarding");
        Ok(false)
    }

    async fn env_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        value: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let accepted = match self.slots.get_mut(&channel) {
            Some(slot) if !slot.started && name == "GIT_PROTOCOL" => protocol_versions(value)
                .is_some_and(|versions| {
                    slot.versions = versions;
                    true
                }),
            _ => false,
        };
        if accepted {
            session.channel_success(channel)?;
        } else {
            session.channel_failure(channel)?;
        }
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        command: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Some(taken) = self.take(channel) else {
            self.refuse_channel("second request on a channel");
            session.channel_failure(channel)?;
            return Ok(());
        };
        session.channel_success(channel)?;
        let ctx = Arc::clone(&self.ctx);
        let command = std::str::from_utf8(command).map(str::to_owned).ok();
        self.tasks.spawn(async move {
            run_exec(&ctx, taken.channel, command, taken.versions).await;
            ctx.open.remove(channel);
        });
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Some(taken) = self.take(channel) else {
            self.refuse_channel("second request on a channel");
            session.channel_failure(channel)?;
            return Ok(());
        };
        session.channel_success(channel)?;
        let ctx = Arc::clone(&self.ctx);
        self.tasks.spawn(async move {
            run_shell(&ctx, taken.channel).await;
            ctx.open.remove(channel);
        });
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _: &str,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.refuse_channel("terminal");
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        _: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.refuse_channel("subsystem");
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn x11_request(
        &mut self,
        channel: ChannelId,
        _: bool,
        _: &str,
        _: &str,
        _: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.refuse_channel("x11");
        session.channel_failure(channel)?;
        Ok(())
    }
}

/// A channel taken out of its slot to run a command on.
struct Taken {
    channel: Channel<Msg>,
    versions: Vec<u8>,
}

impl Conn {
    /// The channel of `id` if nothing was started on it yet; marks it started.
    fn take(&mut self, id: ChannelId) -> Option<Taken> {
        let slot = self.slots.get_mut(&id)?;
        if slot.started {
            return None;
        }
        slot.started = true;
        Some(Taken {
            channel: slot.channel.take()?,
            versions: std::mem::take(&mut slot.versions),
        })
    }
}

fn prohibited(what: &str) -> ChannelOpenFailure {
    ChannelOpenFailure::Other {
        code: 1,
        reason: format!("puddle: {what} is not available through Puddle's Git over SSH"),
    }
}

/// The versions of a `GIT_PROTOCOL` value (`version=2`, `version=1:version=2`); `None` if any part
/// is not one of `version=0`, `version=1`, `version=2`.
fn protocol_versions(value: &str) -> Option<Vec<u8>> {
    let versions = value
        .split(':')
        .map(|part| match part {
            "version=0" => Some(0),
            "version=1" => Some(1),
            "version=2" => Some(2),
            _ => None,
        })
        .collect::<Option<Vec<u8>>>()?;
    (!versions.is_empty() && versions.len() <= 3).then_some(versions)
}

/// The `GIT_PROTOCOL` an upstream request carries: version 0 is the absence of it.
fn git_protocol(versions: &[u8]) -> Option<String> {
    let named: Vec<String> = versions
        .iter()
        .filter(|v| **v != 0)
        .map(|v| format!("version={v}"))
        .collect();
    (!named.is_empty()).then(|| named.join(":"))
}

/// How a command on a channel ended: the exit status, and what to say on the error stream first.
struct Finish {
    status: u32,
    say: Option<String>,
}

impl Finish {
    fn ok() -> Self {
        Self {
            status: 0,
            say: None,
        }
    }

    fn says(status: u32, line: &str) -> Self {
        Self {
            status,
            say: Some(format!("puddle: {line}\n")),
        }
    }
}

async fn run_exec(
    ctx: &Ctx,
    mut channel: Channel<Msg>,
    command: Option<String>,
    versions: Vec<u8>,
) {
    let finish = match command.as_deref().map(|c| grammar::parse(c, &ctx.host)) {
        None | Some(Err(Refused::Unsupported)) => {
            ctx.summary()
                .refusal(HttpRequestLine::new("ssh-command", "/"), UNSUPPORTED);
            Finish::says(
                1,
                "this command is not supported over SSH; Puddle serves git-upload-pack and git-receive-pack (ssh_command_unsupported)",
            )
        }
        Some(Err(Refused::BadPath)) => {
            ctx.summary()
                .refusal(HttpRequestLine::new("ssh-command", "/"), BAD_PATH);
            Finish::says(
                1,
                "this repository path is not one Puddle reads; use owner/repository (bad_git_path)",
            )
        }
        Some(Ok(Command::LfsTransfer)) => Finish {
            // As a host without it answers, so Git LFS falls back to its HTTPS address.
            status: 127,
            say: None,
        },
        Some(Ok(Command::LfsAuthenticate { repository, upload })) => {
            lfs_authenticate(ctx, &channel, &repository, upload).await
        }
        Some(Ok(Command::Git(service, repository))) => {
            git_command(ctx, &mut channel, service, &repository, &versions).await
        }
    };
    end(&channel, finish).await;
}

/// `git-lfs-authenticate`: the table decides as for the LFS request that follows, and the answer is
/// the repository's HTTPS LFS address with no credential in it; the injector adds the credential
/// to the requests that go there.
async fn lfs_authenticate(
    ctx: &Ctx,
    channel: &Channel<Msg>,
    repository: &Repository,
    upload: bool,
) -> Finish {
    let service = if upload {
        grammar::GitService::Receive
    } else {
        grammar::GitService::Upload
    };
    let name = if upload {
        "git-lfs-upload"
    } else {
        "git-lfs-download"
    };
    ctx.summary()
        .command(HttpRequestLine::new(name, &repository.audit_path()));
    match table(ctx, repository, service).await {
        Err(refusal) => refused(ctx, name, repository, &refusal),
        Ok(binding) => {
            ctx.summary().credential(binding.as_deref());
            let href = format!(
                "{{\"href\":\"https://{}{}/info/lfs\"}}\n",
                repository.host, repository.path
            );
            // best effort: the guest may have gone away, and the refusal is already logged
            let _ = channel.data_bytes(bytes::Bytes::from(href)).await;
            Finish::ok()
        }
    }
}

async fn git_command(
    ctx: &Ctx,
    channel: &mut Channel<Msg>,
    service: grammar::GitService,
    repository: &Repository,
    versions: &[u8],
) -> Finish {
    let line = HttpRequestLine::new(service.command(), &repository.audit_path());
    ctx.summary().command(line);
    let binding = match table(ctx, repository, service).await {
        Ok(binding) => binding,
        Err(refusal) => return refused(ctx, service.command(), repository, &refusal),
    };
    ctx.summary().credential(binding.as_deref());
    let command = GitSshCommand {
        host: repository.host.clone(),
        service,
        path: repository.path.clone(),
        git_protocol: git_protocol(versions),
        binding_id: binding,
    };
    match pump(ctx, channel, &command).await {
        Ok(()) => Finish::ok(),
        Err(failure) => Finish::says(1, &failure.message),
    }
}

/// What the repository table says about `service` on `repository`: the credential's binding when
/// the request may go on, else the refusal. It asks the injector about the
/// HTTPS request that stands for the command, so the table, its switches, the choice of identity
/// and the refusal words are the HTTPS ones.
async fn table(
    ctx: &Ctx,
    repository: &Repository,
    service: grammar::GitService,
) -> Result<Option<String>, InjectRefusal> {
    let target = format!(
        "{}/info/refs?service={}",
        repository.path,
        service.http_service()
    );
    let view = RequestView::new("GET", &target, &[]);
    let context = InjectContext {
        workspace: &ctx.workspace,
        host: &repository.host,
    };
    match ctx.injector.decide(&context, &view).await {
        InjectDecision::Inject(injection) => Ok(Some(injection.binding_id().to_owned())),
        InjectDecision::PassThrough
        | InjectDecision::PassThroughOwnCredential
        | InjectDecision::PassThroughNoting(_) => Ok(None),
        InjectDecision::Refuse(refusal) => Err(refusal),
    }
}

/// Bytes the pump holds in each direction between the channel and the upstream (256 KiB).
const PUMP_BUFFER: usize = 262_144;

/// Whether the bytes the guest sent with its `CONNECT` name libssh2 (its identification line
/// does), the client behind libgit2 and so cargo's built-in Git.
fn names_libssh2(early: &[u8]) -> bool {
    early.windows(7).any(|w| w == b"libssh2")
}

/// The SSH server's settings: its own identification, `keys` as the host keys, a failed login
/// answered at once (nothing to slow down: the guest has no password to guess) and no
/// inactivity timeout (a long clone is idle on this side while the upstream works).
fn server_config(keys: Vec<super::GitSshHostKey>) -> russh::server::Config {
    russh::server::Config {
        server_id: russh::SshId::Standard("SSH-2.0-Puddle".into()),
        keys,
        auth_rejection_time: std::time::Duration::ZERO,
        auth_rejection_time_initial: Some(std::time::Duration::ZERO),
        inactivity_timeout: None,
        ..Default::default()
    }
}

/// Records the refusal in the connection's record and words it for the guest.
fn refused(ctx: &Ctx, service: &str, repository: &Repository, refusal: &InjectRefusal) -> Finish {
    ctx.summary().refusal(
        HttpRequestLine::new(service, &repository.audit_path()),
        refusal.code(),
    );
    Finish::says(1, refusal.message())
}

/// Runs the upstream between the channel and the command, counting nothing the guest sent beyond
/// what the channel carries. The upstream sees a plain stream; the channel's error stream, exit
/// status and close stay here.
async fn pump(
    ctx: &Ctx,
    channel: &mut Channel<Msg>,
    command: &GitSshCommand,
) -> Result<(), super::GitSshFailure> {
    let (mut mine, mut theirs) = tokio::io::duplex(PUMP_BUFFER);
    let mut out = channel.make_writer();
    let upstream = Arc::clone(&ctx.upstream);
    let run = async move {
        let result = upstream.run(&ctx.route, command, &mut theirs).await;
        drop(theirs);
        result
    };
    let (mut from_upstream, mut to_upstream) = {
        let (r, w) = tokio::io::split(&mut mine);
        (r, w)
    };
    let to_guest = async {
        let copied = tokio::io::copy(&mut from_upstream, &mut out).await;
        // the guest side may have closed; the copy result is what the caller reports
        let _ = out.flush().await;
        copied
    };
    let mut input = channel.make_reader();
    let from_guest = async {
        // the guest ending its input is the normal end of a push; only the upstream shutdown matters
        let _ = tokio::io::copy(&mut input, &mut to_upstream).await;
        let _ = to_upstream.shutdown().await;
    };
    let main = async { tokio::join!(run, to_guest) };
    let (mut from_guest, mut main) = (pin!(from_guest), pin!(main));
    // The guest finishing its side says nothing about the command, which ends when its own side
    // does; a copy that fails is the guest's channel ending under it, and the command's own result
    // says how it went.
    let mut guest_done = false;
    loop {
        tokio::select! {
            (result, _copied) = &mut main => return result,
            () = &mut from_guest, if !guest_done => guest_done = true,
        }
    }
}

async fn run_shell(ctx: &Ctx, channel: Channel<Msg>) {
    let greeting = ctx.upstream.greeting(&ctx.route, &ctx.host.https).await;
    end(
        &channel,
        Finish {
            status: greeting.status,
            say: Some(greeting.message),
        },
    )
    .await;
}

/// Says what is left to say on the error stream and ends the channel with `finish`'s status.
async fn end(channel: &Channel<Msg>, finish: Finish) {
    if let Some(line) = finish.say {
        let _ = channel
            .extended_data_bytes(1, bytes::Bytes::from(line))
            .await;
    }
    // the guest may have gone away; the channel is finished either way
    let _ = channel.exit_status(finish.status).await;
    let _ = channel.eof().await;
    let _ = channel.close().await;
}

#[cfg(test)]
mod tests;
