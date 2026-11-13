// SPDX-License-Identifier: GPL-3.0-or-later
//! The HTTPS bridge: one SSH command carried out as the smart-HTTP requests `git` itself sends
//! for the HTTPS remote (gitprotocol-http).
//!
//! - The advertisement is one `GET <repo>/info/refs?service=...`, with the smart-HTTP
//!   `# service=` line taken off, since SSH has none.
//! - Protocol version 2 is stateless by design: each command the client sends, up to its flush,
//!   is one `POST`.
//! - A push is one `POST git-receive-pack` with the commands and, when one follows, the pack up to
//!   the client's end of input, streamed. A push that only deletes sends no pack and keeps its
//!   input open for the report, so the commands alone are the request.
//! - A protocol-version-0 fetch is stateful over SSH and stateless over HTTP. Each round of
//!   `have` lines the client ends with a flush (or `done`) becomes one `POST` holding the `want`
//!   block and every `have` so far, which is what `git fetch-pack --stateless-rpc` sends. A
//!   shallow fetch is refused there: the server answers its `deepen` before the first round, which
//!   a stateless request cannot do.

use std::io;

use bytes::Bytes;
use http::header::{ACCEPT, CONTENT_TYPE, HOST, USER_AGENT};
use http::{Method, StatusCode};
use http_body_util::{BodyExt as _, Full};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use super::pkt::{Line, Pkt, PktReader, service_header_len};
use super::{GitService, GitSshCommand, GitSshFailure, GitSshStream};
use crate::destination::BoxFuture;
use crate::terminate::h1::{BoxError, ChannelBody, UpBody};

/// What the bridge sends as `User-Agent`: hosts serve smart HTTP to an agent that starts with
/// `git/`.
const AGENT: &str = "git/puddle-ssh";

/// Most bytes of a refusal's body read for its message.
const REFUSAL_BYTES: usize = 4096;

/// Largest piece of a pack read from the guest at once.
const PIECE: usize = 65_536;

/// Most bytes of one buffered request (a fetch round, or a push's commands) held for the
/// `POST`. The guest chooses the lines, and every guest shares this process, so the buffer has
/// a limit. A pack is streamed and not counted.
const MAX_REQUEST: usize = 16 * 1024 * 1024;

/// Appends `bytes` to the request being built; `elsewhere` is what the same `POST` holds in
/// other buffers.
fn add(into: &mut Vec<u8>, elsewhere: usize, bytes: &[u8]) -> Result<(), GitSshFailure> {
    if (into.len() + elsewhere).saturating_add(bytes.len()) > MAX_REQUEST {
        return Err(GitSshFailure::new(format!(
            "the Git client sent more than {} MiB in one request, more than Puddle carries over SSH",
            MAX_REQUEST / (1024 * 1024)
        )));
    }
    into.extend_from_slice(bytes);
    Ok(())
}

/// The smart-HTTP endpoints of one repository, as the bridge reaches them.
pub(crate) trait SmartHttp: Send {
    /// Sends `request` and returns the answer, or why there is none.
    fn send(
        &mut self,
        request: http::Request<UpBody>,
    ) -> BoxFuture<'_, Result<http::Response<UpBody>, GitSshFailure>>;
}

/// Carries out `command` between `io` and `http`.
pub(crate) async fn carry(
    http: &mut dyn SmartHttp,
    command: &GitSshCommand,
    io: &mut dyn GitSshStream,
) -> Result<(), GitSshFailure> {
    let (read, mut write) = tokio::io::split(io);
    let mut input = PktReader::new(read);
    let target = format!(
        "{}/info/refs?service={}",
        command.path,
        command.service.http_service()
    );
    let advert = http
        .send(request(command, Method::GET, &target, UpBody::default())?)
        .await?;
    copy(accepted(advert, command).await?, &mut write, true).await?;
    match command.service {
        GitService::Receive => push(http, command, &mut input, &mut write).await,
        GitService::Upload if speaks_v2(command) => {
            fetch_v2(http, command, &mut input, &mut write).await
        }
        GitService::Upload => fetch_v0(http, command, &mut input, &mut write).await,
    }
}

/// Whether the client asked for protocol version 2, which `git-upload-pack` speaks.
fn speaks_v2(command: &GitSshCommand) -> bool {
    command
        .git_protocol
        .as_deref()
        .is_some_and(|value| value.split(':').any(|v| v == "version=2"))
}

/// The request for `target` (a path and query) on the command's host.
fn request(
    command: &GitSshCommand,
    method: Method,
    target: &str,
    body: UpBody,
) -> Result<http::Request<UpBody>, GitSshFailure> {
    let post = method == Method::POST;
    let mut builder = http::Request::builder()
        .method(method)
        .uri(target)
        .header(HOST, command.host.to_string())
        .header(USER_AGENT, AGENT);
    if let Some(protocol) = &command.git_protocol {
        builder = builder.header("git-protocol", protocol);
    }
    if post {
        let service = command.service.http_service();
        builder = builder
            .header(CONTENT_TYPE, format!("application/x-{service}-request"))
            .header(ACCEPT, format!("application/x-{service}-result"));
    }
    builder
        .body(body)
        .map_err(|err| GitSshFailure::new(format!("the request for {target} is not valid: {err}")))
}

fn bytes_body(bytes: Bytes) -> UpBody {
    Full::new(bytes)
        .map_err(|never| match never {})
        .boxed_unsync()
}

/// The `POST` of the command's service with `body`.
fn post(command: &GitSshCommand, body: UpBody) -> Result<http::Request<UpBody>, GitSshFailure> {
    let target = format!("{}/{}", command.path, command.service.command());
    request(command, Method::POST, &target, body)
}

/// `answer` when it is a `200`; otherwise the failure that says why, in the words of a refusal
/// Puddle gave (its `puddle: ` line) or with the status the host gave.
async fn accepted(
    answer: http::Response<UpBody>,
    command: &GitSshCommand,
) -> Result<http::Response<UpBody>, GitSshFailure> {
    let status = answer.status();
    if status == StatusCode::OK {
        return Ok(answer);
    }
    let mut body = answer.into_body();
    let mut text = Vec::new();
    while let Some(Ok(frame)) = body.frame().await {
        if let Ok(data) = frame.into_data() {
            text.extend_from_slice(&data);
        }
        if text.len() > REFUSAL_BYTES {
            // Longer than any refusal of Puddle's (one line): the status says what happened.
            text.clear();
            break;
        }
    }
    let text = String::from_utf8_lossy(&text);
    let said = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("puddle: "))
        .map(str::to_owned);
    Err(GitSshFailure::new(said.unwrap_or_else(|| {
        format!(
            "the Git host answered {status} for {}{}",
            command.host, command.path
        )
    })))
}

/// Copies an answer's body to the guest; with `strip`, without the advertisement's `# service=`
/// line.
async fn copy<W: AsyncWrite + Unpin>(
    answer: http::Response<UpBody>,
    out: &mut W,
    strip: bool,
) -> Result<(), GitSshFailure> {
    let mut body = answer.into_body();
    let mut start = Vec::new();
    let mut stripping = strip;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|err| upstream_broke(&*err))?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        if !stripping {
            out.write_all(&data).await.map_err(|err| guest_gone(&err))?;
            continue;
        }
        start.extend_from_slice(&data);
        if let Some(skip) = service_header_len(&start) {
            stripping = false;
            out.write_all(start.get(skip..).unwrap_or_default())
                .await
                .map_err(|err| guest_gone(&err))?;
        }
    }
    if stripping {
        out.write_all(&start)
            .await
            .map_err(|err| guest_gone(&err))?;
    }
    out.flush().await.map_err(|err| guest_gone(&err))
}

fn upstream_broke(err: &(dyn std::error::Error + Send + Sync)) -> GitSshFailure {
    GitSshFailure::new(format!("the answer from the Git host broke off: {err}"))
}

fn guest_gone(err: &io::Error) -> GitSshFailure {
    GitSshFailure::new(format!("the guest's side of the channel closed: {err}"))
}

fn not_git(err: &io::Error) -> GitSshFailure {
    GitSshFailure::new(format!(
        "what the Git client sent is not Git's protocol: {err}"
    ))
}

/// Sends `body` as the service's `POST` and copies the answer to the guest.
async fn round<W: AsyncWrite + Unpin>(
    http: &mut dyn SmartHttp,
    command: &GitSshCommand,
    body: UpBody,
    out: &mut W,
) -> Result<(), GitSshFailure> {
    let answer = http.send(post(command, body)?).await?;
    copy(accepted(answer, command).await?, out, false).await
}

/// Protocol version 2: each request up to its flush is one `POST`; a lone flush, or the end of
/// input, ends the session.
async fn fetch_v2<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    http: &mut dyn SmartHttp,
    command: &GitSshCommand,
    input: &mut PktReader<R>,
    out: &mut W,
) -> Result<(), GitSshFailure> {
    loop {
        let mut request = Vec::new();
        loop {
            match input.next().await.map_err(|err| not_git(&err))? {
                None => return Ok(()),
                Some(Line {
                    pkt: Pkt::Flush,
                    raw,
                }) => {
                    if request.is_empty() {
                        return Ok(());
                    }
                    add(&mut request, 0, &raw)?;
                    break;
                }
                Some(line) => add(&mut request, 0, &line.raw)?,
            }
        }
        round(http, command, bytes_body(Bytes::from(request)), out).await?;
    }
}

/// Protocol version 0 (and 1): the `want` block, then rounds of `have` lines, each one `POST`.
async fn fetch_v0<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    http: &mut dyn SmartHttp,
    command: &GitSshCommand,
    input: &mut PktReader<R>,
    out: &mut W,
) -> Result<(), GitSshFailure> {
    let mut wants = Vec::new();
    loop {
        match input.next().await.map_err(|err| not_git(&err))? {
            None => return Ok(()),
            Some(Line {
                pkt: Pkt::Flush,
                raw,
            }) => {
                if wants.is_empty() {
                    // Nothing wanted: the client is up to date and says so with a flush.
                    return Ok(());
                }
                add(&mut wants, 0, &raw)?;
                break;
            }
            Some(Line {
                pkt: Pkt::Data(data),
                raw,
            }) => {
                if data.starts_with(b"deepen") || data.starts_with(b"shallow ") {
                    return Err(GitSshFailure::new(
                        "a shallow fetch over SSH needs Git protocol version 2 through Puddle; \
                         run it with `git -c protocol.version=2` or use the HTTPS remote",
                    ));
                }
                add(&mut wants, 0, &raw)?;
            }
            Some(_) => return Err(not_git(&unexpected())),
        }
    }
    let mut haves = Vec::new();
    loop {
        let done = loop {
            match input.next().await.map_err(|err| not_git(&err))? {
                // The client gave up between rounds: nothing more to ask.
                None => return Ok(()),
                Some(Line {
                    pkt: Pkt::Flush, ..
                }) => break false,
                Some(Line {
                    pkt: Pkt::Data(data),
                    ..
                }) if data == b"done\n" || data == b"done" => break true,
                Some(line) => add(&mut haves, wants.len(), &line.raw)?,
            }
        };
        let mut body = wants.clone();
        body.extend_from_slice(&haves);
        // The closing line is a few bytes past the limit at most.
        body.extend_from_slice(if done { b"0009done\n" } else { b"0000" });
        round(http, command, bytes_body(Bytes::from(body)), out).await?;
        if done {
            return Ok(());
        }
    }
}

fn unexpected() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "a line Git does not send here")
}

/// What a push's command section says.
#[derive(Debug, Default, PartialEq, Eq)]
struct Commands {
    /// How many refs it updates.
    count: usize,
    /// Whether one of them is not a delete, so a pack follows.
    pack: bool,
    /// Whether the client asked for push options, which follow as a section of their own.
    options: bool,
}

impl Commands {
    /// Reads one line of the command section (`<old> <new> <ref>`, the first with the
    /// capabilities after a NUL; `shallow` lines before them).
    fn read(&mut self, line: &[u8]) -> Result<(), GitSshFailure> {
        if line.starts_with(b"shallow ") {
            return Ok(());
        }
        if line.starts_with(b"push-cert") {
            return Err(GitSshFailure::new(
                "a signed push (`git push --signed`) is not carried over SSH through Puddle; \
                 push over the HTTPS remote",
            ));
        }
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let (update, capabilities) = match line.iter().position(|b| *b == 0) {
            Some(nul) => {
                let (update, rest) = line.split_at(nul);
                (update, rest.get(1..).unwrap_or_default())
            }
            None => (line, &b""[..]),
        };
        let mut fields = update.split(|b| *b == b' ');
        let (Some(old), Some(new), Some(name), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(not_git(&unexpected()));
        };
        if !is_id(old) || !is_id(new) || name.is_empty() {
            return Err(not_git(&unexpected()));
        }
        self.count += 1;
        self.pack |= new.iter().any(|b| *b != b'0');
        self.options |= capabilities
            .split(|b| *b == b' ')
            .any(|word| word == b"push-options");
        Ok(())
    }
}

/// An object id: 40 (SHA-1) or 64 (SHA-256) lower-case hex digits.
fn is_id(text: &[u8]) -> bool {
    matches!(text.len(), 40 | 64) && text.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Reads lines up to and including a flush into `into`; `None` when the input ends first.
async fn section<R: AsyncRead + Unpin>(
    input: &mut PktReader<R>,
    into: &mut Vec<u8>,
    mut each: impl FnMut(&[u8]) -> Result<(), GitSshFailure>,
) -> Result<Option<()>, GitSshFailure> {
    loop {
        match input.next().await.map_err(|err| not_git(&err))? {
            None => return Ok(None),
            Some(line) => {
                add(into, 0, &line.raw)?;
                match line.pkt {
                    Pkt::Flush => return Ok(Some(())),
                    Pkt::Data(data) => each(&data)?,
                    Pkt::Delim | Pkt::ResponseEnd => return Err(not_git(&unexpected())),
                }
            }
        }
    }
}

/// A push: the command section (and push options), then the pack when one follows, as one
/// streamed `POST`.
async fn push<R: AsyncRead + Unpin + Send, W: AsyncWrite + Unpin>(
    http: &mut dyn SmartHttp,
    command: &GitSshCommand,
    input: &mut PktReader<R>,
    out: &mut W,
) -> Result<(), GitSshFailure> {
    let mut head = Vec::new();
    let mut commands = Commands::default();
    if section(input, &mut head, |line| commands.read(line))
        .await?
        .is_none()
        || commands.count == 0
    {
        // Nothing to update ("Everything up-to-date"): no request, as over HTTPS.
        return Ok(());
    }
    if commands.options && section(input, &mut head, |_| Ok(())).await?.is_none() {
        return Ok(());
    }
    if !commands.pack {
        return round(http, command, bytes_body(Bytes::from(head)), out).await;
    }
    let (body, sender, abort) = ChannelBody::new(crate::http::Body::Chunked);
    let feed = async move {
        if sender.send(Bytes::from(head)).await.is_err() {
            return;
        }
        let mut piece = vec![0_u8; PIECE];
        loop {
            match input.get_mut().read(&mut piece).await {
                Ok(0) => return,
                Ok(n) => {
                    let sent = Bytes::copy_from_slice(piece.get(..n).unwrap_or_default());
                    if sender.send(sent).await.is_err() {
                        return;
                    }
                }
                Err(_) => {
                    abort.abort();
                    return;
                }
            }
        }
    };
    let request = post(command, body.map_err(BoxError::from).boxed_unsync())?;
    let (answer, ()) = tokio::join!(http.send(request), feed);
    copy(accepted(answer?, command).await?, out, false).await
}

#[cfg(test)]
mod tests;
