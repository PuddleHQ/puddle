// SPDX-License-Identifier: AGPL-3.0-or-later
//! The bridge against a recording smart-HTTP host: which requests a session turns into, with
//! which bodies and headers, and what the guest reads back.

use std::collections::VecDeque;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::StreamBody;
use puddle_types::Host;

use std::pin::Pin;
use std::task::{Context, Poll};

use super::*;

/// A request as the host got it.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// What the host answers next.
enum Reply {
    Ok(&'static [u8]),
    Status(StatusCode, &'static str),
    /// A `200` whose body breaks off after these bytes.
    Broken(&'static [u8]),
    Fail(&'static str),
}

#[derive(Default)]
struct Host2 {
    seen: Vec<Seen>,
    replies: VecDeque<Reply>,
}

impl Host2 {
    fn answering(replies: impl IntoIterator<Item = Reply>) -> Self {
        Self {
            seen: Vec::new(),
            replies: replies.into_iter().collect(),
        }
    }

    fn bodies(&self) -> Vec<String> {
        self.seen
            .iter()
            .map(|s| String::from_utf8_lossy(&s.body).into_owned())
            .collect()
    }

    fn targets(&self) -> Vec<String> {
        self.seen
            .iter()
            .map(|s| format!("{} {}", s.method, s.target))
            .collect()
    }
}

impl SmartHttp for Host2 {
    fn send(
        &mut self,
        request: http::Request<UpBody>,
    ) -> BoxFuture<'_, Result<http::Response<UpBody>, GitSshFailure>> {
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let body = body.collect().await.unwrap().to_bytes().to_vec();
            self.seen.push(Seen {
                method: parts.method.to_string(),
                target: parts.uri.to_string(),
                headers: parts
                    .headers
                    .iter()
                    .map(|(n, v)| (n.to_string(), v.to_str().unwrap().to_owned()))
                    .collect(),
                body,
            });
            let reply = self.replies.pop_front().expect("a reply for every request");
            let (status, body): (StatusCode, UpBody) = match reply {
                Reply::Ok(bytes) => (StatusCode::OK, bytes_body(Bytes::from_static(bytes))),
                Reply::Status(status, text) => {
                    (status, bytes_body(Bytes::from_static(text.as_bytes())))
                }
                Reply::Broken(bytes) => {
                    let frames: Vec<Result<http_body::Frame<Bytes>, BoxError>> = vec![
                        Ok(http_body::Frame::data(Bytes::from_static(bytes))),
                        Err("connection reset".into()),
                    ];
                    (
                        StatusCode::OK,
                        StreamBody::new(futures_util::stream::iter(frames)).boxed_unsync(),
                    )
                }
                Reply::Fail(message) => return Err(GitSshFailure::new(message)),
            };
            Ok(http::Response::builder().status(status).body(body).unwrap())
        })
    }
}

fn command(service: GitService, protocol: Option<&str>) -> GitSshCommand {
    GitSshCommand {
        host: Host::parse_normalised("bound.test").unwrap(),
        service,
        path: "/acme/web.git".to_owned(),
        git_protocol: protocol.map(str::to_owned),
        binding_id: None,
    }
}

const ADVERT_V0: &[u8] =
    b"001e# service=git-upload-pack\n0000003f1111111111111111111111111111111111111111 refs/heads/main\n0000";
const ADVERT_V0_SSH: &str = "003f1111111111111111111111111111111111111111 refs/heads/main\n0000";

/// Runs the bridge with `input` from the guest (then the end of input) and returns what the guest
/// read and the result.
async fn session(
    host: &mut Host2,
    command: &GitSshCommand,
    input: &[u8],
) -> (String, Result<(), GitSshFailure>) {
    let (mut guest, mut ours) = tokio::io::duplex(input.len().max(1 << 20));
    guest.write_all(input).await.unwrap();
    guest.shutdown().await.unwrap();
    let result = carry(host, command, &mut ours).await;
    drop(ours);
    let mut out = Vec::new();
    guest.read_to_end(&mut out).await.unwrap();
    (String::from_utf8_lossy(&out).into_owned(), result)
}

#[tokio::test]
async fn protocol_v2_is_one_post_per_command_and_a_lone_flush_ends_it() {
    let mut host = Host2::answering([
        Reply::Ok(b"000eversion 2\n0013ls-refs=unborn\n0000"),
        Reply::Ok(b"0032abcd refs/heads/main\n0000"),
        Reply::Ok(b"000dpackfile\n0000"),
    ]);
    let command = command(GitService::Upload, Some("version=2"));
    let input = b"0014command=ls-refs\n00010009peel\n00000012command=fetch\n0000";
    let (out, result) = session(&mut host, &command, &[&input[..], b"0000"].concat()).await;
    result.unwrap();
    assert_eq!(
        out,
        "000eversion 2\n0013ls-refs=unborn\n00000032abcd refs/heads/main\n0000000dpackfile\n0000"
    );
    assert_eq!(
        host.targets(),
        [
            "GET /acme/web.git/info/refs?service=git-upload-pack",
            "POST /acme/web.git/git-upload-pack",
            "POST /acme/web.git/git-upload-pack",
        ]
    );
    assert_eq!(
        host.bodies(),
        [
            "",
            "0014command=ls-refs\n00010009peel\n0000",
            "0012command=fetch\n0000"
        ]
    );
    let get = &host.seen[0];
    assert_eq!(get.header("host"), Some("bound.test"));
    assert_eq!(get.header("user-agent"), Some("git/puddle-ssh"));
    assert_eq!(get.header("git-protocol"), Some("version=2"));
    assert_eq!(get.header("content-type"), None);
    let post = &host.seen[1];
    assert_eq!(
        post.header("content-type"),
        Some("application/x-git-upload-pack-request")
    );
    assert_eq!(
        post.header("accept"),
        Some("application/x-git-upload-pack-result")
    );
    assert_eq!(post.header("git-protocol"), Some("version=2"));
}

#[tokio::test]
async fn protocol_v2_ends_with_the_input_even_inside_a_command() {
    let mut host = Host2::answering([Reply::Ok(b"000eversion 2\n0000")]);
    let command = command(GitService::Upload, Some("version=1:version=2"));
    let (out, result) = session(&mut host, &command, b"0014command=ls-refs\n").await;
    result.unwrap();
    assert_eq!(out, "000eversion 2\n0000");
    assert_eq!(host.seen.len(), 1);
}

#[tokio::test]
async fn protocol_v0_sends_the_wants_and_every_have_so_far_in_each_round() {
    let mut host = Host2::answering([
        Reply::Ok(ADVERT_V0),
        Reply::Ok(b"0008NAK\n"),
        Reply::Ok(b"0031ACK 2222222222222222222222222222222222222222\n"),
        Reply::Ok(b"0008NAK\nPACK"),
    ]);
    let command = command(GitService::Upload, None);
    let want = "0032want 1111111111111111111111111111111111111111\n";
    let have_a = "0032have aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n";
    let have_b = "0032have bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n";
    let input = format!("{want}0000{have_a}0000{have_b}00000009done\n");
    let (out, result) = session(&mut host, &command, input.as_bytes()).await;
    result.unwrap();
    assert_eq!(
        out,
        format!(
            "{ADVERT_V0_SSH}0008NAK\n0031ACK 2222222222222222222222222222222222222222\n0008NAK\nPACK"
        )
    );
    assert_eq!(
        host.bodies()[1..],
        [
            format!("{want}0000{have_a}0000"),
            format!("{want}0000{have_a}{have_b}0000"),
            format!("{want}0000{have_a}{have_b}0009done\n"),
        ]
    );
    assert_eq!(
        host.seen[0].header("git-protocol"),
        None,
        "version 0 sends none"
    );
}

#[tokio::test]
async fn protocol_v0_clone_is_the_wants_and_done_and_a_bare_done_counts() {
    let mut host = Host2::answering([Reply::Ok(ADVERT_V0), Reply::Ok(b"0008NAK\nPACK")]);
    let command = command(GitService::Upload, Some("version=1"));
    let want = "0032want 1111111111111111111111111111111111111111\n";
    let (_, result) = session(
        &mut host,
        &command,
        format!("{want}00000008done").as_bytes(),
    )
    .await;
    result.unwrap();
    assert_eq!(host.bodies()[1], format!("{want}00000009done\n"));
    assert_eq!(host.seen[1].header("git-protocol"), Some("version=1"));
}

#[tokio::test]
async fn protocol_v0_with_nothing_wanted_or_a_client_that_gives_up_asks_nothing_more() {
    for input in [
        &b""[..],
        b"0000",
        b"0032want 1111111111111111111111111111111111111111\n",
        b"0032want 1111111111111111111111111111111111111111\n0000",
    ] {
        let mut host = Host2::answering([Reply::Ok(ADVERT_V0)]);
        let (out, result) = session(&mut host, &command(GitService::Upload, None), input).await;
        result.unwrap();
        assert_eq!(out, ADVERT_V0_SSH, "{input:?}");
        assert_eq!(host.seen.len(), 1, "{input:?}");
    }
}

#[tokio::test]
async fn a_shallow_fetch_on_protocol_v0_is_refused_before_anything_is_asked() {
    for line in [
        "000cdeepen 1\n",
        "0035shallow 1111111111111111111111111111111111111111\n",
    ] {
        let mut host = Host2::answering([Reply::Ok(ADVERT_V0)]);
        let input = format!("0032want 1111111111111111111111111111111111111111\n{line}0000");
        let (_, result) = session(
            &mut host,
            &command(GitService::Upload, None),
            input.as_bytes(),
        )
        .await;
        let failure = result.unwrap_err();
        assert!(
            failure
                .message
                .contains("shallow fetch over SSH needs Git protocol version 2"),
            "{failure:?}"
        );
        assert_eq!(host.seen.len(), 1);
    }
}

#[tokio::test]
async fn protocol_v0_refuses_a_line_git_does_not_send_in_the_want_block() {
    let mut host = Host2::answering([Reply::Ok(ADVERT_V0)]);
    let (_, result) = session(&mut host, &command(GitService::Upload, None), b"0001").await;
    assert!(result.unwrap_err().message.contains("not Git's protocol"));
    let mut host = Host2::answering([Reply::Ok(ADVERT_V0)]);
    let (_, result) = session(&mut host, &command(GitService::Upload, None), b"00zz").await;
    assert!(result.unwrap_err().message.contains("not Git's protocol"));
}

const OLD: &str = "1111111111111111111111111111111111111111";
const NEW: &str = "2222222222222222222222222222222222222222";
const ZERO: &str = "0000000000000000000000000000000000000000";

fn pkt(text: &str) -> String {
    format!("{:04x}{text}", text.len() + 4)
}

const ADVERT_PUSH: &[u8] = b"001f# service=git-receive-pack\n0000003f1111111111111111111111111111111111111111 refs/heads/main\n0000";

#[tokio::test]
async fn a_push_is_one_post_of_the_commands_and_the_pack_to_the_end_of_input() {
    let mut host = Host2::answering([
        Reply::Ok(ADVERT_PUSH),
        Reply::Ok(b"0030\x01000eunpack ok\n0019ok refs/heads/main\n00000000"),
    ]);
    let commands = format!(
        "{}{}0000",
        pkt(&format!(
            "{OLD} {NEW} refs/heads/main\0report-status side-band-64k\n"
        )),
        pkt(&format!("{ZERO} {NEW} refs/heads/new\n")),
    );
    let pack = vec![b'P'; 200_000];
    let input = [commands.as_bytes(), &pack].concat();
    let (out, result) = session(&mut host, &command(GitService::Receive, None), &input).await;
    result.unwrap();
    assert!(out.starts_with("003f1111"), "{out}");
    assert!(out.ends_with("ok refs/heads/main\n00000000"), "{out}");
    assert_eq!(host.seen[1].body, input, "the whole input, in order");
    assert_eq!(
        host.targets(),
        [
            "GET /acme/web.git/info/refs?service=git-receive-pack",
            "POST /acme/web.git/git-receive-pack"
        ]
    );
    assert_eq!(
        host.seen[1].header("content-type"),
        Some("application/x-git-receive-pack-request")
    );
}

#[tokio::test]
async fn a_push_that_only_deletes_goes_up_without_waiting_for_the_end_of_input() {
    let mut host = Host2::answering([Reply::Ok(ADVERT_PUSH), Reply::Ok(b"report")]);
    let commands = format!(
        "{}0000",
        pkt(&format!("{OLD} {ZERO} refs/heads/gone\0report-status\n"))
    );
    let (mut guest, mut ours) = tokio::io::duplex(1 << 16);
    guest.write_all(commands.as_bytes()).await.unwrap();
    // The guest keeps its side open for the report; a bridge that waited for its end would never
    // return, and the bound fails the test instead.
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        carry(&mut host, &command(GitService::Receive, None), &mut ours),
    )
    .await
    .expect("the bridge waited for an end of input that never comes");
    result.unwrap();
    assert_eq!(host.bodies()[1], commands);
}

#[tokio::test]
async fn push_options_are_read_as_a_section_of_their_own() {
    let mut host = Host2::answering([Reply::Ok(ADVERT_PUSH), Reply::Ok(b"report")]);
    let input = format!(
        "{}0000{}0000",
        pkt(&format!(
            "{OLD} {ZERO} refs/heads/gone\0report-status push-options\n"
        )),
        pkt("ci.skip\n"),
    );
    let (mut guest, mut ours) = tokio::io::duplex(1 << 16);
    guest.write_all(input.as_bytes()).await.unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        carry(&mut host, &command(GitService::Receive, None), &mut ours),
    )
    .await
    .expect("the bridge waited for an end of input that never comes");
    result.unwrap();
    assert_eq!(host.bodies()[1], input);
}

#[tokio::test]
async fn a_push_with_nothing_to_update_asks_nothing() {
    for input in [
        String::new(),
        "0000".to_owned(),
        pkt(&format!("{OLD} {NEW} refs/heads/main\0report-status\n")),
        format!(
            "{}0000",
            pkt(&format!("{OLD} {ZERO} refs/heads/gone\0push-options\n"))
        ),
    ] {
        let mut host = Host2::answering([Reply::Ok(ADVERT_PUSH)]);
        let (_, result) = session(
            &mut host,
            &command(GitService::Receive, None),
            input.as_bytes(),
        )
        .await;
        result.unwrap();
        assert_eq!(host.seen.len(), 1, "{input:?}");
    }
}

#[tokio::test]
async fn a_push_command_git_does_not_write_is_refused_and_nothing_goes_up() {
    for (line, said) in [
        (format!("{OLD} {NEW}\n"), "not Git's protocol"),
        (
            format!("{OLD} {NEW} refs/heads/a extra\n"),
            "not Git's protocol",
        ),
        (format!("{OLD} {NEW} \n"), "not Git's protocol"),
        (format!("{OLD} 22 refs/heads/a\n"), "not Git's protocol"),
        (
            format!("{} {NEW} refs/heads/a\n", "A".repeat(40)),
            "not Git's protocol",
        ),
        ("push-cert\0report-status\n".to_owned(), "signed push"),
    ] {
        let mut host = Host2::answering([Reply::Ok(ADVERT_PUSH)]);
        let input = format!("{}0000", pkt(&line));
        let (_, result) = session(
            &mut host,
            &command(GitService::Receive, None),
            input.as_bytes(),
        )
        .await;
        let failure = result.unwrap_err();
        assert!(failure.message.contains(said), "{line:?}: {failure:?}");
        assert_eq!(host.seen.len(), 1, "{line:?}");
    }
    let mut host = Host2::answering([Reply::Ok(ADVERT_PUSH)]);
    let (_, result) = session(&mut host, &command(GitService::Receive, None), b"0001").await;
    assert!(result.unwrap_err().message.contains("not Git's protocol"));
}

#[test]
fn a_command_line_says_how_many_refs_whether_a_pack_follows_and_whether_options_do() {
    let mut commands = Commands::default();
    commands
        .read(format!("shallow {OLD}\n").as_bytes())
        .unwrap();
    assert_eq!(commands, Commands::default());
    commands
        .read(format!("{OLD} {ZERO} refs/heads/a\0report-status\n").as_bytes())
        .unwrap();
    assert_eq!(
        commands,
        Commands {
            count: 1,
            pack: false,
            options: false
        }
    );
    let sha256_new = "3".repeat(64);
    commands
        .read(format!("{} {sha256_new} refs/heads/b", "0".repeat(64)).as_bytes())
        .unwrap();
    assert_eq!(
        commands,
        Commands {
            count: 2,
            pack: true,
            options: false
        }
    );
    let mut options = Commands::default();
    options
        .read(format!("{OLD} {ZERO} refs/heads/a\0push-options atomic\n").as_bytes())
        .unwrap();
    assert!(options.options && !options.pack);
    let mut plain = Commands::default();
    plain
        .read(format!("{OLD} {ZERO} refs/heads/push-options\n").as_bytes())
        .unwrap();
    assert!(
        !plain.options,
        "a ref named push-options is not the capability"
    );
}

#[test]
fn an_object_id_is_forty_or_sixty_four_lower_case_hex_digits() {
    assert!(is_id(OLD.as_bytes()));
    assert!(is_id("a".repeat(64).as_bytes()));
    assert!(is_id(b"0123456789abcdef0123456789abcdef01234567"));
    for bad in [
        "1".repeat(39),
        "1".repeat(41),
        "1".repeat(63),
        "A".repeat(40),
        "g".repeat(40),
        String::new(),
    ] {
        assert!(!is_id(bad.as_bytes()), "{bad}");
    }
}

#[tokio::test]
async fn a_refusal_puddle_gave_reaches_the_guest_in_its_own_words() {
    let mut host = Host2::answering([Reply::Status(
        StatusCode::FORBIDDEN,
        "\npuddle: push to bound.test/acme/web is not on this workspace's push list\n",
    )]);
    let (out, result) = session(&mut host, &command(GitService::Receive, None), b"").await;
    assert_eq!(out, "");
    assert_eq!(
        result.unwrap_err().message,
        "push to bound.test/acme/web is not on this workspace's push list"
    );
}

#[tokio::test]
async fn a_refusal_from_the_host_names_its_status_and_the_repository() {
    let mut host = Host2::answering([Reply::Status(StatusCode::NOT_FOUND, "Repository not found")]);
    let (_, result) = session(&mut host, &command(GitService::Upload, None), b"").await;
    assert_eq!(
        result.unwrap_err().message,
        "the Git host answered 404 Not Found for bound.test/acme/web.git"
    );
    // A refusal on a later request, the same way.
    let mut host = Host2::answering([
        Reply::Ok(b"000eversion 2\n0000"),
        Reply::Status(StatusCode::UNAUTHORIZED, ""),
    ]);
    let (_, result) = session(
        &mut host,
        &command(GitService::Upload, Some("version=2")),
        b"0014command=ls-refs\n0000",
    )
    .await;
    assert_eq!(
        result.unwrap_err().message,
        "the Git host answered 401 Unauthorized for bound.test/acme/web.git"
    );
}

#[tokio::test]
async fn a_refusal_body_up_to_the_limit_is_read_for_puddles_words_and_a_longer_one_is_not() {
    static EXACT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    static OVER: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let exact = EXACT.get_or_init(|| format!("puddle: no\n{}", " ".repeat(REFUSAL_BYTES - 11)));
    let over = OVER.get_or_init(|| format!("{exact} "));
    assert_eq!(exact.len(), REFUSAL_BYTES);
    let mut host = Host2::answering([Reply::Status(StatusCode::FORBIDDEN, exact)]);
    let (_, result) = session(&mut host, &command(GitService::Upload, None), b"").await;
    assert_eq!(result.unwrap_err().message, "no");
    let mut host = Host2::answering([Reply::Status(StatusCode::FORBIDDEN, over)]);
    let (_, result) = session(&mut host, &command(GitService::Upload, None), b"").await;
    assert_eq!(
        result.unwrap_err().message,
        "the Git host answered 403 Forbidden for bound.test/acme/web.git"
    );
}

#[tokio::test]
async fn only_the_start_of_a_long_refusal_is_read() {
    struct Endless(Option<UpBody>);
    impl SmartHttp for Endless {
        fn send(
            &mut self,
            _: http::Request<UpBody>,
        ) -> BoxFuture<'_, Result<http::Response<UpBody>, GitSshFailure>> {
            let body = self.0.take().unwrap();
            Box::pin(async move {
                Ok(http::Response::builder()
                    .status(StatusCode::BAD_GATEWAY)
                    .body(body)
                    .unwrap())
            })
        }
    }

    // A refusal body that never ends: the bridge stops after its limit instead of reading on.
    let endless = futures_util::stream::repeat_with(|| {
        Ok::<_, BoxError>(http_body::Frame::data(Bytes::from_static(&[b'x'; 1024])))
    });
    let mut host = Endless(Some(StreamBody::new(endless).boxed_unsync()));
    let (mut guest, mut ours) = tokio::io::duplex(1 << 16);
    guest.shutdown().await.unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        carry(&mut host, &command(GitService::Upload, None), &mut ours),
    )
    .await
    .expect("the bridge read an endless refusal to its end");
    assert_eq!(
        result.unwrap_err().message,
        "the Git host answered 502 Bad Gateway for bound.test/acme/web.git"
    );
}

#[tokio::test]
async fn an_answer_that_breaks_off_or_a_host_that_cannot_be_reached_is_a_failure() {
    let mut host = Host2::answering([Reply::Broken(b"000eversion 2\n")]);
    let (out, result) = session(
        &mut host,
        &command(GitService::Upload, Some("version=2")),
        b"",
    )
    .await;
    assert_eq!(
        out, "000eversion 2\n",
        "what came before the break is passed on"
    );
    assert!(
        result
            .unwrap_err()
            .message
            .starts_with("the answer from the Git host broke off: connection reset"),
    );
    let mut host = Host2::answering([Reply::Fail("bound.test cannot be reached")]);
    let (_, result) = session(&mut host, &command(GitService::Upload, None), b"").await;
    assert_eq!(result.unwrap_err().message, "bound.test cannot be reached");
}

#[tokio::test]
async fn an_advertisement_too_short_to_hold_the_service_line_is_passed_on_as_it_is() {
    let mut host = Host2::answering([Reply::Ok(b"00")]);
    let (out, result) = session(&mut host, &command(GitService::Upload, None), b"").await;
    result.unwrap();
    assert_eq!(out, "00");
}

#[tokio::test]
async fn a_guest_that_is_gone_ends_the_command() {
    let mut host = Host2::answering([Reply::Ok(ADVERT_V0)]);
    let (guest, mut ours) = tokio::io::duplex(16);
    drop(guest);
    let failure = carry(&mut host, &command(GitService::Upload, None), &mut ours)
        .await
        .unwrap_err();
    assert!(
        failure
            .message
            .starts_with("the guest's side of the channel closed"),
        "{failure:?}"
    );
}

#[tokio::test]
async fn a_pack_that_breaks_off_is_not_sent_as_a_complete_body() {
    /// A guest stream whose read fails once the commands are read.
    struct Failing {
        input: io::Cursor<Vec<u8>>,
    }
    impl AsyncRead for Failing {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let pos = usize::try_from(self.input.position()).unwrap();
            let rest = self.input.get_ref().len() - pos;
            if rest == 0 {
                return Poll::Ready(Err(io::Error::other("cut")));
            }
            let n = rest.min(buf.remaining());
            let start = pos;
            buf.put_slice(&self.input.get_ref()[start..start + n]);
            self.input.set_position(u64::try_from(start + n).unwrap());
            Poll::Ready(Ok(()))
        }
    }
    impl AsyncWrite for Failing {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }
    /// Answers the advertisement, then reads the push body and says how it ended.
    struct Reading(usize);
    impl SmartHttp for Reading {
        fn send(
            &mut self,
            request: http::Request<UpBody>,
        ) -> BoxFuture<'_, Result<http::Response<UpBody>, GitSshFailure>> {
            self.0 += 1;
            let first = self.0 == 1;
            Box::pin(async move {
                if first {
                    return Ok(http::Response::new(bytes_body(Bytes::from_static(
                        ADVERT_PUSH,
                    ))));
                }
                let ended = match request.into_body().collect().await {
                    Ok(_) => "complete",
                    Err(_) => "cut off",
                };
                Err(GitSshFailure::new(ended))
            })
        }
    }
    let commands = format!("{}0000", pkt(&format!("{OLD} {NEW} refs/heads/main\n")));
    let mut io = Failing {
        input: io::Cursor::new(commands.into_bytes()),
    };
    let failure = carry(
        &mut Reading(0),
        &command(GitService::Receive, None),
        &mut io,
    )
    .await
    .unwrap_err();
    assert_eq!(failure.message, "cut off");
}

#[tokio::test]
async fn a_request_the_bridge_cannot_build_is_a_failure_and_never_sent() {
    let mut host = Host2::default();
    let mut bad = command(GitService::Upload, None);
    bad.path = "/acme/we b.git".to_owned();
    let (_, result) = session(&mut host, &bad, b"").await;
    assert!(
        result
            .unwrap_err()
            .message
            .starts_with("the request for /acme/we b.git/info/refs"),
    );
    assert!(host.seen.is_empty());
}

#[test]
fn version_2_is_spoken_only_when_the_client_names_it() {
    assert!(speaks_v2(&command(GitService::Upload, Some("version=2"))));
    assert!(speaks_v2(&command(
        GitService::Upload,
        Some("version=1:version=2")
    )));
    assert!(!speaks_v2(&command(GitService::Upload, Some("version=1"))));
    assert!(!speaks_v2(&command(GitService::Upload, None)));
}

#[tokio::test]
async fn trailers_in_an_answer_are_not_bytes_for_the_guest() {
    struct Trailing;
    impl SmartHttp for Trailing {
        fn send(
            &mut self,
            _: http::Request<UpBody>,
        ) -> BoxFuture<'_, Result<http::Response<UpBody>, GitSshFailure>> {
            let frames: Vec<Result<http_body::Frame<Bytes>, BoxError>> = vec![
                Ok(http_body::Frame::data(Bytes::from_static(
                    b"000eversion 2\n",
                ))),
                Ok(http_body::Frame::trailers(http::HeaderMap::new())),
                Ok(http_body::Frame::data(Bytes::from_static(b"0000"))),
            ];
            let body = StreamBody::new(futures_util::stream::iter(frames)).boxed_unsync();
            Box::pin(async move { Ok(http::Response::new(body)) })
        }
    }
    let (mut guest, mut ours) = tokio::io::duplex(1 << 16);
    guest.shutdown().await.unwrap();
    carry(
        &mut Trailing,
        &command(GitService::Upload, Some("version=2")),
        &mut ours,
    )
    .await
    .unwrap();
    drop(ours);
    let mut out = Vec::new();
    guest.read_to_end(&mut out).await.unwrap();
    assert_eq!(out, b"000eversion 2\n0000");
}

/// Answers the advertisement, then takes `frames` frames of the push body and fails.
struct Refusing {
    calls: usize,
    frames: usize,
}

impl SmartHttp for Refusing {
    fn send(
        &mut self,
        request: http::Request<UpBody>,
    ) -> BoxFuture<'_, Result<http::Response<UpBody>, GitSshFailure>> {
        self.calls += 1;
        let first = self.calls == 1;
        let frames = self.frames;
        Box::pin(async move {
            if first {
                return Ok(http::Response::new(bytes_body(Bytes::from_static(
                    ADVERT_PUSH,
                ))));
            }
            let mut body = request.into_body();
            for _ in 0..frames {
                let _ = body.frame().await;
            }
            Err(GitSshFailure::new("refused mid-body"))
        })
    }
}

#[tokio::test]
async fn a_host_that_stops_taking_the_push_ends_it_whatever_the_guest_still_sends() {
    for frames in [0, 1] {
        let commands = format!("{}0000", pkt(&format!("{OLD} {NEW} refs/heads/main\n")));
        let (mut guest, mut ours) = tokio::io::duplex(4 << 20);
        guest.write_all(commands.as_bytes()).await.unwrap();
        // More pack than the body's channel holds, so the bridge is still sending when the host
        // stops; the guest's input stays open, so only the host's refusal can end it.
        guest.write_all(&vec![b'P'; 1 << 20]).await.unwrap();
        let failure = tokio::time::timeout(
            Duration::from_secs(30),
            carry(
                &mut Refusing { calls: 0, frames },
                &command(GitService::Receive, None),
                &mut ours,
            ),
        )
        .await
        .expect("the bridge kept reading after the host stopped taking the push")
        .unwrap_err();
        assert_eq!(failure.message, "refused mid-body", "{frames}");
    }
}

/// Pkt-lines of data with no flush among them, `total` bytes in all (each line at most 65520).
fn unflushed(total: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(total);
    let mut left = total;
    while left > 0 {
        // Never leave a rest too small to be a line.
        let size = match left {
            0..=65_520 => left,
            65_521..=65_524 => 65_515,
            _ => 65_520,
        };
        out.extend_from_slice(format!("{size:04x}").as_bytes());
        out.resize(out.len() + size - 4, b'x');
        left -= size;
    }
    out
}

#[tokio::test]
async fn a_request_the_guest_never_flushes_is_refused_at_the_limit_and_nothing_goes_up() {
    let too_much = unflushed(MAX_REQUEST + 100);
    let commands = format!("{OLD} {NEW} refs/heads/main\0report-status\n");
    // Valid update lines, each as long as a line can be, so the section is only too long.
    let line = pkt(&format!(
        "{OLD} {NEW} refs/heads/{}\n",
        "x".repeat(65_520 - 4 - 1 - 2 * 40 - 2 - "refs/heads/".len())
    ));
    assert_eq!(line.len(), 65_520);
    let long_commands = [
        pkt(&commands).into_bytes(),
        line.repeat(MAX_REQUEST / 65_520 + 1).into_bytes(),
    ]
    .concat();
    let cases: [(GitService, Option<&str>, Vec<u8>); 4] = [
        // Protocol version 2: one command that never ends.
        (GitService::Upload, Some("version=2"), too_much.clone()),
        // Version 0: a want block that never ends, and rounds of haves that never end.
        (GitService::Upload, None, too_much.clone()),
        (
            GitService::Upload,
            None,
            [
                pkt("want 1111111111111111111111111111111111111111\n").as_bytes(),
                b"0000",
                &too_much,
            ]
            .concat(),
        ),
        // A push whose command section never ends.
        (GitService::Receive, None, long_commands),
    ];
    for (service, protocol, input) in cases {
        let advert = if service == GitService::Receive {
            ADVERT_PUSH
        } else {
            ADVERT_V0
        };
        let mut host = Host2::answering([Reply::Ok(advert)]);
        let (_, result) = session(&mut host, &command(service, protocol), &input).await;
        let message = result.unwrap_err().message;
        assert!(
            message.contains("more than 16 MiB"),
            "{service:?}: {message}"
        );
        assert_eq!(
            host.seen.len(),
            1,
            "{service:?}: only the advertisement was asked for"
        );
    }
}

#[tokio::test]
async fn a_request_of_exactly_the_limit_still_goes_up() {
    let mut host = Host2::answering([
        Reply::Ok(b"000eversion 2\n0000"),
        Reply::Ok(b"000dpackfile\n0000"),
    ]);
    let mut input = unflushed(MAX_REQUEST - 4);
    input.extend_from_slice(b"0000");
    let (_, result) = session(
        &mut host,
        &command(GitService::Upload, Some("version=2")),
        &input,
    )
    .await;
    result.unwrap();
    assert_eq!(host.seen[1].body.len(), MAX_REQUEST);
}
