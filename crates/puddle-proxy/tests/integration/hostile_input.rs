// SPDX-License-Identifier: GPL-3.0-or-later
//! A hostile guest sends the proxy malformed heads (a 10 MB line, 100 000 headers, NUL, bytes that
//! are not UTF-8, bad ports and brackets) on a real route. Every refusal names its cause in the
//! response body (so a test passes only for the limit or rule it is about, not because the
//! connection ended), no rule is asked, and a second workspace on the same proxy keeps
//! getting through while the attack runs.
use crate::hostile_support;

use hostile_support::{Guest, assert_unharmed, bystander_round, rig};

/// What a guest may send that is not a head: what it is, the bytes, the status and the reason the
/// refusal gives.
fn malformed_heads() -> Vec<(&'static str, Vec<u8>, u16, &'static str)> {
    let ten_mb_line = [b"GET http://a.test/".to_vec(), vec![b'a'; 10 * 1024 * 1024]].concat();
    let hundred_thousand_headers = format!(
        "GET http://a.test/ HTTP/1.1\r\n{}\r\n",
        "X-A: b\r\n".repeat(100_000)
    )
    .into_bytes();
    vec![
        (
            "a 10 MB request line with no CRLF",
            ten_mb_line,
            431,
            "request head over 64 KiB",
        ),
        (
            "100 000 header lines",
            hundred_thousand_headers,
            431,
            "request head over 64 KiB",
        ),
        (
            "NUL in the request line",
            b"GET http://a\0.test/ HTTP/1.1\r\n\r\n".to_vec(),
            400,
            "control character in head",
        ),
        (
            "NUL in a header",
            b"GET http://a.test/ HTTP/1.1\r\nX: a\0b\r\n\r\n".to_vec(),
            400,
            "control character in head",
        ),
        (
            "bytes that are not UTF-8",
            b"GET http://a.test/\xff\xfe HTTP/1.1\r\n\r\n".to_vec(),
            400,
            "non-UTF-8 head",
        ),
        (
            "a head cut off before its blank line",
            b"CONNECT a.test:443 HTTP/1.1\r\nHost: a.test".to_vec(),
            400,
            "head ended early",
        ),
        (
            "an unclosed bracket",
            b"CONNECT [::1:443 HTTP/1.1\r\n\r\n".to_vec(),
            400,
            "unclosed [ in host",
        ),
        (
            "brackets round a name",
            b"CONNECT [a.test]:443 HTTP/1.1\r\n\r\n".to_vec(),
            400,
            "brackets around a non-IPv6 host",
        ),
        (
            "junk after the bracket",
            b"CONNECT [::1]x HTTP/1.1\r\n\r\n".to_vec(),
            400,
            "junk after ]",
        ),
        (
            "a port over 65535",
            b"CONNECT a.test:65536 HTTP/1.1\r\n\r\n".to_vec(),
            400,
            "port out of range",
        ),
        (
            "port zero",
            b"CONNECT a.test:0 HTTP/1.1\r\n\r\n".to_vec(),
            400,
            "port out of range",
        ),
        (
            "a port that is not a number",
            b"CONNECT a.test:80x HTTP/1.1\r\n\r\n".to_vec(),
            400,
            "port is not a number",
        ),
        (
            "CONNECT with no port",
            b"CONNECT a.test HTTP/1.1\r\n\r\n".to_vec(),
            400,
            "CONNECT needs host:port",
        ),
    ]
}

#[tokio::test]
async fn hostile_hg10_malformed_heads_are_refused_for_their_own_reason() {
    let rig = rig().await;
    let quiet = bystander_round(&rig.bystander, rig.echo).await;
    let mut guest = Guest::connect(&rig.attacked).await;
    let cases = malformed_heads();
    let attack = async {
        for (what, bytes, code, cause) in cases {
            let (got, body) = guest.refused(bytes.into()).await;
            assert_eq!(
                (got, body.contains(cause)),
                (code, true),
                "{what}: {code} {body:?}"
            );
        }
    };
    let (timed, ()) = tokio::join!(bystander_round(&rig.bystander, rig.echo), attack);
    assert_unharmed(timed, quiet, "while the other workspace was attacked");
    assert_eq!(
        rig.policy.decisions(),
        128,
        "only the bystander's connections reached the rules"
    );
    // The attacked workspace still works too: the refusals did not poison its session.
    let still = bystander_round(&rig.attacked, rig.echo).await;
    assert_unharmed(still, quiet, "on the attacked workspace afterwards");
}
