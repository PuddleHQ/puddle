// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hostile-guest cases for Git over SSH: the guest names a repository another way, asks for more
//! than Git, attacks the SSH server before and around authentication, and lies in the Git stream.
//! Each case ends with what reached the Git host:
//! nothing, or requests only for the repository the command named.

use super::*;

use tokio::io::{AsyncRead, AsyncWrite};

/// The identification a real client sends.
const BANNER: &[u8] = b"SSH-2.0-OpenSSH_10.0\r\n";

/// Runs `command` with `stdin`, tolerating a server that closes the channel before all of it is
/// written (the refusal of a stream that is too long comes mid-way).
async fn exec_any(
    handle: &Handle<Trusting>,
    env: &[(&str, &str)],
    command: &str,
    stdin: &[u8],
) -> Ran {
    let mut channel = handle.channel_open_session().await.unwrap();
    for (name, value) in env {
        channel.set_env(true, *name, *value).await.unwrap();
    }
    channel.exec(true, command).await.unwrap();
    let _ = channel.data(stdin).await;
    let _ = channel.eof().await;
    collect(&mut channel, env.len() + 1).await
}

/// A pkt-line carrying `text`.
fn pkt(text: &str) -> String {
    format!("{:04x}{text}", text.len() + 4)
}

/// The requests the Git host saw, as `METHOD target`.
fn asked(t: &SshRig) -> Vec<String> {
    t.git
        .server
        .recorded()
        .iter()
        .map(|r| format!("{} {}", r.method, r.target))
        .collect()
}

const V2: &[(&str, &str)] = &[("GIT_PROTOCOL", "version=2")];

// ---------------------------------------------------------------------------------------------
// Another spelling of an unlisted repository.

#[tokio::test]
async fn hostile_hg47_every_other_spelling_of_a_command_is_refused_before_a_byte_goes_upstream() {
    let (t, _) = SshRig::https(smart_host(), GitSshAccount::Unnamed, &[]).await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    for command in [
        // Git's quoting, abused.
        "git-upload-pack 'acme/'\\''other.git'",
        "git-upload-pack 'acme/web.git'\\''",
        "git-upload-pack \"acme/other.git\"",
        "git-upload-pack acme/other.git",
        "git-upload-pack 'acme\\other.git'",
        "git-upload-pack 'acme/other.git",
        "git-upload-pack acme/other.git'",
        "git-upload-pack $'acme/other.git'",
        "git-upload-pack '$(id)/web.git'",
        "git-upload-pack '`id`/web.git'",
        // A second command, in every way a shell would take one.
        "git-upload-pack 'acme/web.git'; git-upload-pack 'acme/other.git'",
        "git-upload-pack 'acme/web.git'&&id",
        "git-upload-pack 'acme/web.git'|id",
        "git-upload-pack 'acme/web.git'\nid",
        "git-upload-pack 'acme/web.git'\rid",
        "git-upload-pack 'acme/web.git' git-upload-pack 'acme/other.git'",
        "git-upload-pack 'acme/web\0.git'",
        "git-upload-pack 'acme/web.git\0'",
        "git-upload-pack 'acme/web.git' 'acme/other.git'",
        // The command word.
        "git receive-pack 'acme/web.git'",
        "git-receive-pack  'acme/web.git'",
        "git-upload-pack\t'acme/web.git'",
        "GIT-UPLOAD-PACK 'acme/web.git'",
        "Git-Upload-Pack 'acme/web.git'",
        "git-upload-pack 'acme/web.git' --strict",
        "git-upload-archive 'acme/web.git'",
        "/usr/bin/git-upload-pack 'acme/web.git'",
        // The path.
        "git-upload-pack '../other.git'",
        "git-upload-pack 'acme/../other/web.git'",
        "git-upload-pack 'acme/./web.git'",
        "git-upload-pack 'acme//other.git'",
        "git-upload-pack '//acme/web.git'",
        "git-upload-pack 'acme/other/'",
        "git-upload-pack 'acme%2Fother.git'",
        "git-upload-pack 'acme/%2e%2e/web.git'",
        "git-upload-pack 'acme/web.git%00'",
        "git-upload-pack '~acme/other.git'",
        "git-upload-pack 'acme/web.git.git'",
        "git-upload-pack 'acme/web.git/info/refs'",
        "git-upload-pack 'acme/web.git?service=git-receive-pack'",
        "git-upload-pack 'acme/web.git#x'",
        "git-upload-pack 'ac me/web.git'",
        "git-upload-pack ''",
        "git-upload-pack '/'",
        "git-upload-pack 'web.git'",
    ] {
        let ran = exec_any(&ssh, &[], command, b"0000").await;
        assert_eq!(ran.exit, Some(1), "{command:?}: {ran:?}");
        assert!(ran.err.starts_with("puddle: "), "{command:?}: {ran:?}");
        assert!(
            ran.err.contains("ssh_command_unsupported") || ran.err.contains("bad_git_path"),
            "{command:?}: {ran:?}"
        );
        assert_eq!(ran.out, "", "{command:?}");
    }
    assert_eq!(asked(&t), <[String; 0]>::default(), "nothing went upstream");
    // The connection is not the worse for it: the named repository still works.
    let ran = exec(&ssh, V2, "git-upload-pack 'acme/web.git'", b"").await;
    assert_eq!(ran.exit, Some(0), "{ran:?}");
    assert_eq!(
        asked(&t),
        ["GET /acme/web.git/info/refs?service=git-upload-pack"]
    );
}

#[tokio::test]
async fn hostile_hg47_an_azure_devops_path_takes_no_escape_but_a_space() {
    let t = SshRig::new().await;
    t.git
        .rig
        .policy
        .allow(&crate::terminate_support::host("ssh.dev.azure.com"));
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "ssh.dev.azure.com:22").await;
    login(&mut ssh).await;
    for path in [
        "v3/org/my%2520project/repo",
        "v3/org/my%2Fproject/repo",
        "v3/org/my%2fproject/repo",
        "v3/org/my%5Cproject/repo",
        "v3/org/%2e%2e/repo",
        "v3/org/proj/repo%0a",
        "v3/org/proj/repo%00",
        "v3/org/proj/..",
        "v3/org/proj/repo/extra",
        "v3/org/proj",
        "v2/org/proj/repo",
        "v3//proj/repo",
        "v3/org/proj/_git/repo",
        "org/proj/repo",
    ] {
        let command = format!("git-upload-pack '{path}'");
        let ran = exec_any(&ssh, &[], &command, b"").await;
        assert_eq!(ran.exit, Some(1), "{command}: {ran:?}");
        assert!(ran.err.contains("bad_git_path"), "{command}: {ran:?}");
    }
    let ran = exec(&ssh, &[], "git-upload-pack 'v3/org/my%20project/repo'", b"").await;
    assert_eq!(ran.exit, Some(0), "{ran:?}");
    let calls = t.upstream.calls();
    assert_eq!(calls.len(), 1, "only the one with %20 went on");
    assert_eq!(calls[0].path, "/org/my%20project/_git/repo");
}

#[tokio::test]
async fn hostile_hg47_a_push_to_an_unlisted_repository_is_denied_in_every_spelling_that_names_it() {
    let t = SshRig::new().await;
    t.git
        .world
        .switches(puddle_inject::testing::OnlyListed::Push);
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    for spelling in [
        "acme/other",
        "acme/other.git",
        "/acme/other.git",
        "/acme/other",
        "ACME/OTHER.git",
        "Acme/Other",
        "acme/OTHER",
    ] {
        let command = format!("git-receive-pack '{spelling}'");
        let ran = exec(&ssh, &[], &command, b"0000").await;
        assert_eq!(ran.exit, Some(1), "{command}: {ran:?}");
        assert!(
            ran.err.starts_with(
                "puddle: Blocked this push: bound.test/acme/other is not in this workspace's allowed repositories for push"
            ),
            "{command}: {ran:?}"
        );
        assert_eq!(ran.out, "", "{command}");
    }
    assert_eq!(t.upstream.calls().len(), 0, "no receive-pack went upstream");
    drop(ssh);
    let events = t.events(1).await;
    assert_eq!(
        events[0].reason,
        ConnectionReason::Refused(ReasonCode::PushDenied)
    );
}

#[tokio::test]
async fn hostile_hg47_the_lfs_commands_name_no_other_repository_either() {
    let t = SshRig::new().await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    for command in [
        "git-lfs-authenticate 'acme/web.git'",
        "git-lfs-authenticate 'acme/web.git' sideways",
        "git-lfs-authenticate 'acme/web.git' download extra",
        "git-lfs-authenticate 'acme/../other.git' download",
        "git-lfs-authenticate 'acme/web.git'; id download",
        "git-lfs-authenticate acme/web.git download",
    ] {
        let ran = exec_any(&ssh, &[], command, b"").await;
        assert_eq!(ran.exit, Some(1), "{command}: {ran:?}");
        assert!(ran.err.starts_with("puddle: "), "{command}: {ran:?}");
        assert!(!ran.out.contains("href"), "{command}: {ran:?}");
    }
    // `git-lfs-transfer` runs nothing whatever follows it: it is answered as a host without it.
    let ran = exec_any(&ssh, &[], "git-lfs-transfer ; id #", b"").await;
    assert_ne!(ran.exit, Some(0), "{ran:?}");
    assert!(!ran.out.contains("uid="), "{ran:?}");
    assert_eq!(t.upstream.calls().len(), 0);
}

// ---------------------------------------------------------------------------------------------
// More than Git.

#[tokio::test]
async fn hostile_hg48_nothing_leaves_but_the_one_connection_for_the_named_repository() {
    let (t, _) = SshRig::https(smart_host(), GitSshAccount::Unnamed, &[]).await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    for (host, port) in [
        ("bound.test", 443),
        ("github.com", 443),
        ("127.0.0.1", 443),
        ("127.0.0.1", 22),
        ("127.0.0.1", 3128),
        ("localhost", 8080),
        ("::1", 22),
        ("169.254.169.254", 80),
    ] {
        let opened = ssh
            .channel_open_direct_tcpip(host, port, "127.0.0.1", 40000)
            .await;
        let err = opened.expect_err("a forward is refused").to_string();
        assert!(err.contains("AdministrativelyProhibited"), "{host}: {err}");
    }
    for (bind, port) in [("127.0.0.1", 2222), ("0.0.0.0", 0), ("::", 8080)] {
        assert!(ssh.tcpip_forward(bind, port).await.is_err(), "{bind}");
    }
    assert!(
        ssh.channel_open_direct_streamlocal("/var/run/docker.sock")
            .await
            .is_err()
    );
    assert!(ssh.streamlocal_forward("/tmp/guest.sock").await.is_err());
    assert!(ssh.channel_open_x11("127.0.0.1", 6000).await.is_err());

    // On a session channel: a terminal, a shell, a subsystem, an agent, X11, a second command
    // and the variables that change what a program loads.
    let mut channel = ssh.channel_open_session().await.unwrap();
    for variable in [
        ("LD_PRELOAD", "/tmp/x.so"),
        ("LD_LIBRARY_PATH", "/tmp"),
        ("GIT_SSH_COMMAND", "id"),
        ("GIT_EXEC_PATH", "/tmp"),
        ("GIT_PROTOCOL", "version=2:version=9"),
        ("GIT_PROTOCOL", "version=9"),
        ("GIT_PROTOCOL", ""),
        ("PATH", "/tmp"),
    ] {
        channel.set_env(true, variable.0, variable.1).await.unwrap();
        assert!(!collect_one(&mut channel).await, "{variable:?}");
    }
    for (kind, accepted) in [
        ("pty", {
            channel
                .request_pty(true, "xterm", 80, 24, 0, 0, &[])
                .await
                .unwrap();
            collect_one(&mut channel).await
        }),
        ("sftp", {
            channel.request_subsystem(true, "sftp").await.unwrap();
            collect_one(&mut channel).await
        }),
        ("netconf", {
            channel.request_subsystem(true, "netconf").await.unwrap();
            collect_one(&mut channel).await
        }),
        ("agent", {
            channel.agent_forward(true).await.unwrap();
            collect_one(&mut channel).await
        }),
        ("x11", {
            channel
                .request_x11(true, false, "MIT-MAGIC-COOKIE-1", "00", 0)
                .await
                .unwrap();
            collect_one(&mut channel).await
        }),
        ("signal", {
            channel.signal(russh::Sig::KILL).await.unwrap();
            false
        }),
    ] {
        assert!(!accepted, "{kind}");
    }
    drop(channel);

    // An `exec` after an `exec` on one channel: the second is refused, the first runs.
    let mut twice = ssh.channel_open_session().await.unwrap();
    twice
        .exec(true, "git-upload-pack 'acme/web.git'")
        .await
        .unwrap();
    twice.exec(true, "id").await.unwrap();
    twice.eof().await.unwrap();
    let ran = collect(&mut twice, 2).await;
    assert_eq!(ran.replies, [true, false], "{ran:?}");
    assert_eq!(ran.exit, Some(0), "{ran:?}");

    // Not one connection to the Git host's address for any of it: only the one command's.
    assert_eq!(
        asked(&t),
        ["GET /acme/web.git/info/refs?service=git-upload-pack"]
    );
    assert_eq!(t.git.server.accepted(), 1);
    // And the connection works after all of it.
    let ran = exec(&ssh, V2, "git-upload-pack 'acme/web.git'", b"").await;
    assert_eq!(ran.exit, Some(0), "{ran:?}");
}

#[tokio::test]
async fn hostile_hg48_the_eleventh_channel_is_refused_and_the_ten_open_go_on() {
    let t = SshRig::new().await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    let mut open = Vec::new();
    for _ in 0..10 {
        let channel = ssh.channel_open_session().await.unwrap();
        channel
            .exec(true, "git-upload-pack 'acme/web.git'")
            .await
            .unwrap();
        open.push(channel);
    }
    for _ in 0..3 {
        assert!(
            ssh.channel_open_session().await.is_err(),
            "the eleventh channel is over the cap"
        );
    }
    // The ten go on and finish, and a place that frees up is given again.
    for channel in &mut open {
        channel.eof().await.unwrap();
        let ran = collect(channel, 1).await;
        assert_eq!(ran.exit, Some(0), "{ran:?}");
    }
    drop(open);
    let ran = exec(&ssh, &[], "git-upload-pack 'acme/web.git'", b"").await;
    assert_eq!(ran.exit, Some(0), "{ran:?}");
    assert_eq!(t.upstream.calls().len(), 11);
}

// ---------------------------------------------------------------------------------------------
// The SSH server before and around authentication.

/// A connection to Puddle's SSH server that is ours to write to: the `CONNECT` sent, nothing
/// else. What comes back (the `200`, then Puddle's banner) is read by the caller.
async fn raw(guest: &Guest) -> impl AsyncRead + AsyncWrite + Unpin + use<> {
    let mut stream = guest.open_stream().await;
    stream
        .write_all(
            format!(
                "CONNECT bound.test:22 HTTP/1.1\r\nHost: bound.test:22\r\n{PROTOCOL_HEADER}: {SSH}\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    stream
}

/// Whether the server ends the connection within a few seconds, whatever is still being sent.
async fn is_ended(stream: &mut (impl AsyncRead + Unpin)) -> bool {
    let mut sink = vec![0_u8; 4096];
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match stream.read(&mut sink).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    })
    .await
    .is_ok()
}

/// A key exchange initialisation as the SSH transport sends it (RFC 4253 section 7.1), in the
/// clear, with the algorithms the server would pick.
fn kexinit() -> Vec<u8> {
    let mut payload = vec![20_u8];
    payload.extend_from_slice(&[7; 16]);
    for names in [
        "curve25519-sha256",
        "ssh-ed25519",
        "aes256-ctr",
        "aes256-ctr",
        "hmac-sha2-256",
        "hmac-sha2-256",
        "none",
        "none",
        "",
        "",
    ] {
        payload.extend_from_slice(&u32::try_from(names.len()).unwrap().to_be_bytes());
        payload.extend_from_slice(names.as_bytes());
    }
    payload.push(0);
    payload.extend_from_slice(&[0; 4]);
    let padding = 8 - (payload.len() + 5) % 8 + 8;
    let length = u32::try_from(1 + payload.len() + padding).unwrap();
    let mut packet = length.to_be_bytes().to_vec();
    packet.push(u8::try_from(padding).unwrap());
    packet.extend_from_slice(&payload);
    packet.extend(std::iter::repeat_n(0, padding));
    packet
}

#[tokio::test]
async fn hostile_hg50_an_identification_line_over_255_bytes_ends_the_connection() {
    let t = SshRig::new().await;
    let mut guest = t.git.rig.guest();
    let mut stream = raw(&guest).await;
    let mut line = b"SSH-2.0-".to_vec();
    line.extend(std::iter::repeat_n(b'A', 300));
    line.extend_from_slice(b"\r\n");
    stream.write_all(&line).await.unwrap();
    assert!(is_ended(&mut stream).await, "the connection stays open");
    // A line with no end at all, sent without stopping, is bounded the same way.
    let mut endless = raw(&guest).await;
    let junk = vec![b'B'; 64 * 1024];
    let sent = tokio::time::timeout(Duration::from_secs(10), async {
        for _ in 0..256 {
            if endless.write_all(&junk).await.is_err() {
                return;
            }
        }
    })
    .await;
    assert!(sent.is_ok(), "the server keeps reading an endless line");
    assert!(is_ended(&mut endless).await);
    assert_eq!(t.upstream.calls().len(), 0);
    // Another connection to the same workspace is served as before.
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    let ran = exec(&ssh, &[], "git-upload-pack 'acme/web.git'", b"").await;
    assert_eq!(ran.exit, Some(0), "{ran:?}");
}

#[tokio::test]
async fn hostile_hg50_a_handshake_that_never_ends_is_closed_while_other_sessions_go_on() {
    let limits = GitSshLimits {
        handshake: Duration::from_secs(3),
        ..GitSshLimits::default()
    };
    let t = SshRig::with(Recording::default(), limits).await;
    let guest = t.git.rig.guest();
    let mut stalled = Vec::new();
    for _ in 0..4 {
        let mut stream = raw(&guest).await;
        stream.write_all(BANNER).await.unwrap();
        stalled.push(stream);
    }
    // A client that finishes in time is served while the others hang.
    let mut quick = t.git.rig.guest();
    let mut ssh = t.client(&mut quick, "bound.test:22").await;
    login(&mut ssh).await;
    let ran = exec(&ssh, &[], "git-upload-pack 'acme/web.git'", b"").await;
    assert_eq!(ran.exit, Some(0), "{ran:?}");
    for stream in &mut stalled {
        assert!(is_ended(stream).await, "a stalled handshake is closed");
    }
}

#[tokio::test]
async fn hostile_hg50_a_huge_packet_or_a_garbage_stream_after_the_banner_ends_the_connection() {
    // A packet that is cut short of its length waits for the rest until the handshake limit.
    let limits = GitSshLimits {
        handshake: Duration::from_secs(2),
        ..GitSshLimits::default()
    };
    let t = SshRig::with(Recording::default(), limits).await;
    let mut guest = t.git.rig.guest();
    // A packet that says it is almost 4 GiB.
    let mut huge = raw(&guest).await;
    huge.write_all(BANNER).await.unwrap();
    huge.write_all(&0xFFFF_FFF0_u32.to_be_bytes())
        .await
        .unwrap();
    huge.write_all(&[8; 64]).await.unwrap();
    assert!(is_ended(&mut huge).await, "a 4 GiB packet is read");
    // A length that is under the SSH minimum, and one past what the transport allows.
    for length in [0_u32, 1, 35_001, 0x0100_0000] {
        let mut stream = raw(&guest).await;
        stream.write_all(BANNER).await.unwrap();
        stream.write_all(&length.to_be_bytes()).await.unwrap();
        stream.write_all(&[6; 64]).await.unwrap();
        assert!(is_ended(&mut stream).await, "length {length}");
    }
    // Megabytes of noise.
    let mut noise = raw(&guest).await;
    noise.write_all(BANNER).await.unwrap();
    let junk = vec![0x5A_u8; 64 * 1024];
    let sent = tokio::time::timeout(Duration::from_secs(10), async {
        for _ in 0..128 {
            if noise.write_all(&junk).await.is_err() {
                return;
            }
        }
    })
    .await;
    assert!(sent.is_ok(), "the server keeps reading noise");
    assert!(is_ended(&mut noise).await);
    assert_eq!(t.upstream.calls().len(), 0);
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    assert_eq!(
        exec(&ssh, &[], "git-upload-pack 'acme/web.git'", b"")
            .await
            .exit,
        Some(0)
    );
}

#[tokio::test]
async fn hostile_hg50_a_key_exchange_storm_ends_the_connection_and_the_next_one_is_served() {
    let t = SshRig::new().await;
    let mut guest = t.git.rig.guest();
    let mut storm = raw(&guest).await;
    storm.write_all(BANNER).await.unwrap();
    let packet = kexinit();
    let sent = tokio::time::timeout(Duration::from_secs(10), async {
        for _ in 0..5000 {
            if storm.write_all(&packet).await.is_err() {
                return;
            }
        }
    })
    .await;
    assert!(sent.is_ok(), "the server keeps taking key exchanges");
    assert!(
        is_ended(&mut storm).await,
        "the storm is not answered for ever"
    );
    assert_eq!(t.upstream.calls().len(), 0);
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    assert_eq!(
        exec(&ssh, &[], "git-upload-pack 'acme/web.git'", b"")
            .await
            .exit,
        Some(0)
    );
}

#[tokio::test]
async fn hostile_hg50_thousands_of_authentication_attempts_are_bounded_and_none_is_logged() {
    let t = SshRig::new().await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    let key = russh::keys::PrivateKey::random(&mut Os, Algorithm::Ed25519).unwrap();
    let key = Arc::new(key);
    for round in 0..3000 {
        let attempt = async {
            match round % 3 {
                0 => ssh
                    .authenticate_password("git", "CANARY-PASSWORD-50")
                    .await
                    .map(|r| r.success()),
                1 => ssh
                    .authenticate_publickey(
                        "CANARY-USER-50",
                        russh::keys::PrivateKeyWithHashAlg::new(Arc::clone(&key), None),
                    )
                    .await
                    .map(|r| r.success()),
                _ => ssh
                    .authenticate_keyboard_interactive_start(
                        "git",
                        Some("CANARY-SUBMETHOD-50".to_owned()),
                    )
                    .await
                    .map(|_| true),
            }
        };
        // A server that stops answering after the first success, or ends the connection, is the
        // bound; one that keeps answering must do it quickly.
        match tokio::time::timeout(Duration::from_millis(300), attempt).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => break,
        }
    }
    drop(ssh);
    // Either the server went on answering, each attempt cheap, or it ended the connection;
    // what it may not do is keep anything of them.
    let events = t.events(1).await;
    let text = format!("{events:?}");
    let logs = crate::terminate_support::captured_logs();
    for canary in [
        "CANARY-PASSWORD-50",
        "CANARY-USER-50",
        "CANARY-SUBMETHOD-50",
    ] {
        assert!(
            !text.contains(canary),
            "{canary} is in the audit or an event"
        );
        assert!(!logs.contains(canary), "{canary} is in a log");
    }
    let key_text = key.public_key().to_openssh().unwrap();
    let blob = key_text.split(' ').nth(1).unwrap();
    assert!(
        !text.contains(blob) && !logs.contains(blob),
        "a key is kept"
    );
    // The workspace is served as before.
    let mut again = t.client(&mut guest, "bound.test:22").await;
    login(&mut again).await;
    assert_eq!(
        exec(&again, &[], "git-upload-pack 'acme/web.git'", b"")
            .await
            .exit,
        Some(0)
    );
}

// ---------------------------------------------------------------------------------------------
// The guest's Git stream lies.

#[tokio::test]
async fn hostile_hg51_a_pkt_line_length_that_lies_ends_the_command_with_exit_status_1() {
    let (t, _) = SshRig::https(smart_host(), GitSshAccount::Unnamed, &[]).await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    let mut past_the_limit = b"fff1".to_vec();
    past_the_limit.extend(std::iter::repeat_n(b'x', 70_000));
    let past_the_data = b"0100short".to_vec();
    let cases: Vec<(&str, &str, Vec<u8>)> = vec![
        (
            "git-upload-pack",
            "length past the limit",
            past_the_limit.clone(),
        ),
        ("git-upload-pack", "ffff", b"ffff0000".to_vec()),
        ("git-upload-pack", "not hex", b"zzzz0000".to_vec()),
        ("git-upload-pack", "sign", b"-001abc".to_vec()),
        ("git-upload-pack", "0003", b"00030000".to_vec()),
        ("git-upload-pack", "past the data", past_the_data.clone()),
        ("git-receive-pack", "length past the limit", past_the_limit),
        ("git-receive-pack", "not hex", b"zzzz0000".to_vec()),
        ("git-receive-pack", "past the data", b"0200short".to_vec()),
        (
            "git-receive-pack",
            "a delimiter in the commands",
            b"00010000".to_vec(),
        ),
        ("git-receive-pack", "a command with a fourth word", {
            let line = format!("{SHA} {SHA} refs/heads/a extra\n");
            format!("{:04x}{line}0000", line.len() + 4).into_bytes()
        }),
        (
            "git-receive-pack",
            "an object id that is not one",
            b"0035xyz 1111111111111111111111111111111111111111 refs/x\n0000".to_vec(),
        ),
    ];
    for (service, what, input) in cases {
        let command = format!("{service} 'acme/web.git'");
        let ran = exec_any(&ssh, V2, &command, &input).await;
        assert_eq!(ran.exit, Some(1), "{service} {what}: {ran:?}");
        assert!(ran.err.starts_with("puddle: "), "{service} {what}: {ran:?}");
        assert!(
            ran.err.contains("not Git's protocol"),
            "{service} {what}: {ran:?}"
        );
    }
    // Only advertisements went upstream: no request carried one of the lies.
    for request in t.git.server.recorded() {
        assert_eq!(request.method, "GET", "{}", request.target);
        assert!(request.target.starts_with("/acme/web.git/info/refs"));
    }
}

#[tokio::test]
async fn hostile_hg51_a_stream_that_stops_mid_line_frees_the_channel_and_the_connection_goes_on() {
    let (t, _) = SshRig::https(smart_host(), GitSshAccount::Unnamed, &[]).await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    for _ in 0..25 {
        let channel = ssh.channel_open_session().await.unwrap();
        channel
            .set_env(true, "GIT_PROTOCOL", "version=2")
            .await
            .unwrap();
        channel
            .exec(true, "git-upload-pack 'acme/web.git'")
            .await
            .unwrap();
        // The start of a line of 65 520 bytes, a part of it, and then silence and a close.
        channel.data(&b"fff0command=fetch\n"[..]).await.unwrap();
        channel.close().await.unwrap();
    }
    // More rounds than the cap of channels: every one was freed.
    let ran = exec(&ssh, V2, "git-upload-pack 'acme/web.git'", b"").await;
    assert_eq!(ran.exit, Some(0), "{ran:?}");
    assert!(
        asked(&t).iter().all(|r| !r.starts_with("POST")),
        "no half request went upstream"
    );
}

#[tokio::test]
async fn hostile_hg51_every_request_goes_to_the_repository_the_command_named() {
    let (t, _) = SshRig::https(smart_host(), GitSshAccount::Unnamed, &[]).await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;

    // A fetch whose requests name another repository in their arguments, twice on one channel.
    let first = format!(
        "{}0001{}{}0000",
        pkt("command=ls-refs\n"),
        pkt("peel\n"),
        pkt("ref-prefix acme/other\n")
    );
    let second = format!(
        "{}0001{}{}{}0000",
        pkt("command=fetch\n"),
        pkt("thin-pack\n"),
        pkt("want 1111111111111111111111111111111111111111\n"),
        pkt("done\n")
    );
    let input = format!("{first}{second}");
    let ran = exec(&ssh, V2, "git-upload-pack 'acme/web.git'", input.as_bytes()).await;
    assert_eq!(ran.exit, Some(0), "{ran:?}");
    let seen = t.git.server.recorded();
    let targets: Vec<String> = seen
        .iter()
        .map(|r| format!("{} {}", r.method, r.target))
        .collect();
    assert_eq!(
        targets,
        [
            "GET /acme/web.git/info/refs?service=git-upload-pack",
            "POST /acme/web.git/git-upload-pack",
            "POST /acme/web.git/git-upload-pack"
        ]
    );
    assert_eq!(seen[1].body, first.as_bytes(), "sent as the guest sent it");
    assert_eq!(seen[2].body, second.as_bytes());

    // A push whose commands name refs that look like another repository's.
    let update = format!(
        "{SHA} 2222222222222222222222222222222222222222 refs/heads/../../other.git/refs/heads/main\0report-status\n"
    );
    let second_update =
        format!("{SHA} 2222222222222222222222222222222222222222 refs/heads/acme/other\n");
    let mut push = format!("{:04x}{update}", update.len() + 4).into_bytes();
    push.extend_from_slice(
        format!("{:04x}{second_update}0000PACK", second_update.len() + 4).as_bytes(),
    );
    let before = seen.len();
    let ran = exec(&ssh, &[], "git-receive-pack 'acme/web.git'", &push).await;
    assert_eq!(ran.exit, Some(0), "{ran:?}");
    let after = t.git.server.recorded();
    for request in &after[before..] {
        assert!(
            request.target.starts_with("/acme/web.git/"),
            "{}",
            request.target
        );
    }
    assert_eq!(after.len(), before + 2, "one advertisement, one push");
    assert_eq!(after[before + 1].body, push);
    assert!(
        after.iter().all(|r| !r.target.contains("other")),
        "nothing for another repository"
    );
}

#[tokio::test]
async fn hostile_hg51_a_request_that_never_flushes_is_cut_off_at_a_bound_before_it_is_sent() {
    let (t, _) = SshRig::https(smart_host(), GitSshAccount::Unnamed, &[]).await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    let over = 17 * 1024 * 1024;
    let lines = |line: &str| line.repeat(over / line.len() + 1).into_bytes();
    let have = format!("0032have {SHA}\n");
    let want = format!("0032want {SHA}\n");
    let update = format!("{SHA} 2222222222222222222222222222222222222222 refs/heads/b\n");
    let command = format!("{:04x}{update}", update.len() + 4);
    for (name, env, service, input) in [
        ("a v2 request", V2, "git-upload-pack", lines(&have)),
        ("a v0 want block", &[][..], "git-upload-pack", lines(&want)),
        (
            "a push's commands",
            &[][..],
            "git-receive-pack",
            lines(&command),
        ),
    ] {
        let ran = exec_any(&ssh, env, &format!("{service} 'acme/web.git'"), &input).await;
        assert_eq!(ran.exit, Some(1), "{name}: {ran:?}");
        assert!(ran.err.contains("16 MiB"), "{name}: {ran:?}");
    }
    assert!(
        asked(&t).iter().all(|r| !r.starts_with("POST")),
        "no request was sent"
    );
}
