// SPDX-License-Identifier: AGPL-3.0-or-later
use proptest::prelude::*;

use super::*;

async fn read(bytes: &[u8]) -> io::Result<ResponseHead> {
    let mut slice = bytes;
    read_response_head(&mut slice).await
}

#[tokio::test]
async fn a_407_with_several_challenges_is_parsed() {
    let head = read(
            b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Negotiate\r\nproxy-authenticate: NTLM\r\nPROXY-AUTHENTICATE: Basic realm=\"x\"\r\nContent-Length: 0\r\n\r\n",
        )
        .await
        .unwrap();
    assert_eq!(head.status, 407);
    assert_eq!(head.reason, "Proxy Authentication Required");
    assert_eq!(
        head.challenges(),
        vec!["Negotiate", "NTLM", "Basic realm=\"x\""]
    );
    assert_eq!(schemes(&head.challenges()), ["Negotiate", "NTLM", "Basic"]);
    assert_eq!(head.content_length(), Some(0));
    assert!(head.keeps_alive());
}

#[tokio::test]
async fn only_the_head_is_consumed() {
    let mut data: &[u8] = b"HTTP/1.1 200 OK\r\n\r\nSSH-2.0-server\r\n";
    let head = read_response_head(&mut data).await.unwrap();
    assert_eq!(head.status, 200);
    assert_eq!(data, b"SSH-2.0-server\r\n");
}

#[tokio::test]
async fn connection_close_and_http10_end_keep_alive() {
    let close = read(b"HTTP/1.1 407 x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    assert!(!close.keeps_alive());
    let old = read(b"HTTP/1.0 407 x\r\n\r\n").await.unwrap();
    assert!(!old.keeps_alive());
    let proxy_close = read(b"HTTP/1.1 407 x\r\nProxy-Connection: Close\r\n\r\n")
        .await
        .unwrap();
    assert!(!proxy_close.keeps_alive());
}

#[tokio::test]
async fn ambiguous_or_missing_lengths_are_none() {
    let two = read(b"HTTP/1.1 407 x\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\n")
        .await
        .unwrap();
    assert_eq!(two.content_length(), None);
    let junk = read(b"HTTP/1.1 407 x\r\nContent-Length: -1\r\n\r\n")
        .await
        .unwrap();
    assert_eq!(junk.content_length(), None);
}

#[tokio::test]
async fn malformed_heads_are_errors_not_panics() {
    for bytes in [
        &b"garbage\r\n\r\n"[..],
        b"HTTP/2 200 OK\r\n\r\n",
        b"HTTP/1.1 abc OK\r\n\r\n",
        b"HTTP/1.1 99 OK\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nno colon\r\n\r\n",
        b"HTTP/1.1 200 OK\r\n\xff\xfe: x\r\n\r\n",
        b"HTTP/1.1 200 OK\r\n",
        b"",
    ] {
        assert!(read(bytes).await.is_err(), "{bytes:?}");
    }
}

#[tokio::test]
async fn an_endless_head_is_cut_off() {
    let mut big = b"HTTP/1.1 200 OK\r\n".to_vec();
    big.extend(std::iter::repeat_n(b'a', MAX_RESPONSE_HEAD * 2));
    let err = read(&big).await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn schemes_are_deduplicated_in_order() {
    assert_eq!(
        schemes(&["NTLM abc", "ntlm", "Basic realm=\"r\"", ""]),
        ["NTLM", "Basic"]
    );
}

proptest! {
    #[test]
    fn parsing_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..600)) {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let _ = rt.block_on(async { read(&bytes).await });
    }
}
