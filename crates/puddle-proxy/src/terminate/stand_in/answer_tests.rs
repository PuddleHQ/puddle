// SPDX-License-Identifier: GPL-3.0-or-later

use http_body_util::BodyExt as _;
use proptest::prelude::*;

use super::*;
use crate::terminate::inject::SecretValue;
use crate::terminate::set::TerminationSet;
use crate::terminate::stand_in::{Placement, StandIn, StandInOrigin};

const GH: &str = "puddle-secret-GH_TOKEN-00112233445566778899aabbccddeeff";
const REAL_GH: &str = "ghp_realRealReal1111";
/// A real value with characters every encoding changes.
const ODD: &str = "puddle-secret-ODD-ffeeddccbbaa99887766554433221100";
const REAL_ODD: &str = "a/b c+d\"e\\f<g>&é=k";
/// A captured login: the stand-in has the token's length and alphabet.
const LOGIN: &str = "gho_StandInStandInStandInStandIn1234";
const REAL_LOGIN: &str = "gho_RealTokenRealTokenRealTokenR5678";

fn host(name: &str) -> Host {
    Host::parse_normalised(name).unwrap()
}

fn entry(origin: StandInOrigin, name: &str, text: &str, real: &str, hosts: &[&str]) -> StandIn {
    StandIn::new(
        origin,
        name,
        text,
        SecretValue::new(real),
        TerminationSet::parse(hosts.iter().copied()).unwrap(),
    )
    .unwrap()
}

fn registry() -> StandIns {
    let registry = StandIns::new();
    for (name, text, real, hosts) in [
        ("GH_TOKEN", GH, REAL_GH, &["api.github.com"][..]),
        (
            "ODD",
            ODD,
            REAL_ODD,
            &["api.github.com", "odd.example.com"][..],
        ),
    ] {
        registry
            .insert(entry(StandInOrigin::Secret, name, text, real, hosts))
            .unwrap();
    }
    registry
}

fn scrubber() -> Scrubber {
    registry().answer_scrubber(&host("api.github.com")).unwrap()
}

/// What the guest gets of `pieces`, fed one at a time as a body's frames are.
fn stream(scrubber: &Scrubber, pieces: &[&[u8]]) -> Result<Vec<u8>, Withheld> {
    let notes = ScrubNotes::default();
    let mut held = Vec::new();
    let mut out = Vec::new();
    for piece in pieces {
        held.extend_from_slice(piece);
        let used = scrubber.run(&held, false, &mut out, &notes)?;
        held.drain(..used);
    }
    let used = scrubber.run(&held, true, &mut out, &notes)?;
    assert_eq!(used, held.len(), "the end flushes everything");
    Ok(out)
}

fn whole(scrubber: &Scrubber, input: &[u8]) -> Vec<u8> {
    stream(scrubber, &[input]).unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[test]
fn every_form_of_a_real_value_comes_back_as_the_stand_in_in_the_same_form() {
    let scrubber = scrubber();
    let cases: Vec<(String, String)> = vec![
        (REAL_GH.to_owned(), GH.to_owned()),
        (REAL_ODD.to_owned(), ODD.to_owned()),
        // A whole URL component, either hex case, and a form's `+`.
        (
            "a%2Fb%20c%2Bd%22e%5Cf%3Cg%3E%26%C3%A9%3Dk".to_owned(),
            ODD.to_owned(),
        ),
        (
            "a%2fb%20c%2bd%22e%5cf%3cg%3e%26%c3%a9%3dk".to_owned(),
            ODD.to_owned(),
        ),
        (
            "a%2Fb+c%2Bd%22e%5Cf%3Cg%3E%26%C3%A9%3Dk".to_owned(),
            ODD.to_owned(),
        ),
        // JSON: plain, `\/`, ASCII-only, HTML-safe.
        (r#"a/b c+d\"e\\f<g>&é=k"#.to_owned(), ODD.to_owned()),
        (r#"a\/b c+d\"e\\f<g>&é=k"#.to_owned(), ODD.to_owned()),
        (r#"a/b c+d\"e\\f<g>&é=k"#.to_owned(), ODD.to_owned()),
        (r#"a/b c+d\"e\\f<g>&é=k"#.to_owned(), ODD.to_owned()),
        // base64 in both alphabets, with and without padding.
        (STANDARD.encode(REAL_ODD), STANDARD.encode(ODD)),
        (
            URL_SAFE_NO_PAD.encode(REAL_ODD),
            URL_SAFE_NO_PAD.encode(ODD),
        ),
        (STANDARD.encode(REAL_GH), STANDARD.encode(GH)),
    ];
    for (form, expected) in cases {
        let body = format!("<p>echo: {form} end</p>");
        assert_eq!(
            text(&whole(&scrubber, body.as_bytes())),
            format!("<p>echo: {expected} end</p>"),
            "{form}"
        );
    }
}

#[test]
fn the_value_inside_longer_base64_cuts_the_answer_off_before_it() {
    let scrubber = scrubber();
    let cores = base64_cores(REAL_GH);
    for lead in ["", "x", "xy", "x-access-token:"] {
        let blob = STANDARD.encode(format!("{lead}{REAL_GH}:tail"));
        if lead.len() % 3 == 0 {
            // At the first alignment the value's own base64 is in the text: it is replaced (the
            // text no longer decodes to what it was, but holds no real value).
            let out = whole(&scrubber, blob.as_bytes());
            assert!(
                !cores
                    .iter()
                    .any(|core| out.windows(core.len()).any(|w| w == &core[..]))
            );
            continue;
        }
        let body = format!("{{\"authorization\":\"Basic {blob}\"}}");
        let notes = ScrubNotes::default();
        let mut out = Vec::new();
        let cut = scrubber.run(body.as_bytes(), true, &mut out, &notes);
        assert!(cut.is_err(), "{lead:?}");
        assert!(notes.withheld());
        assert!(
            body.as_bytes().starts_with(&out),
            "only what came before it"
        );
        assert!(!text(&out).contains(&blob[blob.len() / 2..]));
    }
    // A real value too short for its core to be told from other base64 is not looked for there.
    let registry = StandIns::new();
    registry
        .insert(entry(
            StandInOrigin::Secret,
            "PIN",
            "puddle-secret-PIN-00000000000000000000000000000001",
            "4711abc",
            &["api.github.com"],
        ))
        .unwrap();
    let short = registry.answer_scrubber(&host("api.github.com")).unwrap();
    let blob = STANDARD.encode("xy4711abcdef");
    assert_eq!(whole(&short, blob.as_bytes()), blob.as_bytes());
}

#[test]
fn only_a_tail_that_could_become_a_form_waits_for_the_next_piece() {
    let scrubber = scrubber();
    let notes = ScrubNotes::default();
    let mut out = Vec::new();
    // An event of a stream goes out whole.
    let event = b"data: {\"delta\":\"hello\"}\n\n";
    assert_eq!(
        scrubber.run(event, false, &mut out, &notes).unwrap(),
        event.len()
    );
    assert_eq!(out, event);
    // The start of a real value waits; the rest does not.
    out.clear();
    let piece = b"data: ghp_real";
    let used = scrubber.run(piece, false, &mut out, &notes).unwrap();
    assert_eq!(&piece[..used], b"data: ");
    // So does a value split across pieces: it is found whole.
    assert_eq!(
        text(&stream(&scrubber, &[b"token=ghp_re", b"alReal", b"Real1111;"]).unwrap()),
        format!("token={GH};")
    );
}

#[test]
fn only_the_stand_ins_for_the_host_are_looked_for() {
    let registry = registry();
    assert!(registry.answer_scrubber(&host("example.com")).is_none());
    assert!(!registry.scrubs_answers_from(&host("example.com")));
    assert!(registry.scrubs_answers_from(&host("odd.example.com")));
    let odd = registry.answer_scrubber(&host("odd.example.com")).unwrap();
    let body = format!("{REAL_GH} {REAL_ODD}");
    assert_eq!(
        text(&whole(&odd, body.as_bytes())),
        format!("{REAL_GH} {ODD}")
    );
    // A query or path secret is put back too: it is placed elsewhere, but the host saw it.
    let placed = StandIns::new();
    placed
        .insert(
            entry(
                StandInOrigin::Secret,
                "BOT",
                GH,
                REAL_GH,
                &["api.github.com"],
            )
            .placed(Placement::Path),
        )
        .unwrap();
    let scrubber = placed.answer_scrubber(&host("api.github.com")).unwrap();
    assert_eq!(text(&whole(&scrubber, REAL_GH.as_bytes())), GH);
}

#[test]
fn a_stand_in_with_the_tokens_shape_keeps_the_bodys_length() {
    let registry = StandIns::new();
    registry
        .insert(entry(
            StandInOrigin::CapturedLogin,
            "github-access",
            LOGIN,
            REAL_LOGIN,
            &["api.github.com"],
        ))
        .unwrap();
    let login = registry.answer_scrubber(&host("api.github.com")).unwrap();
    assert!(login.same_length);
    assert!(!scrubber().same_length, "a generic stand-in is longer");
    let body = format!("{{\"token\":\"{REAL_LOGIN}\"}}");
    let out = whole(&login, body.as_bytes());
    assert_eq!(out.len(), body.len());
    assert_eq!(text(&out), format!("{{\"token\":\"{LOGIN}\"}}"));
}

#[test]
fn answer_headers_get_the_stand_in_back_also_in_cookies_redirects_and_basic() {
    let scrubber = scrubber();
    let notes = ScrubNotes::default();
    let mut headers = HeaderMap::new();
    headers.append(
        "set-cookie",
        format!("t={REAL_GH}; Path=/; Secure").parse().unwrap(),
    );
    headers.append(
        "location",
        format!("https://api.github.com/cb?token={REAL_GH}")
            .parse()
            .unwrap(),
    );
    headers.append(
        "x-echo",
        format!("Basic {}", STANDARD.encode(format!("git:{REAL_GH}")))
            .parse()
            .unwrap(),
    );
    headers.append(
        "x-blob",
        STANDARD.encode(format!("x{REAL_GH}tail")).parse().unwrap(),
    );
    headers.append("x-plain", "nothing".parse().unwrap());
    scrubber.scrub_headers(&mut headers, &notes);
    assert_eq!(headers["set-cookie"], format!("t={GH}; Path=/; Secure"));
    assert_eq!(
        headers["location"],
        format!("https://api.github.com/cb?token={GH}")
    );
    assert_eq!(
        headers["x-echo"],
        format!("Basic {}", STANDARD.encode(format!("git:{GH}")))
    );
    assert_eq!(
        headers["x-blob"], "",
        "cannot be replaced, so it is emptied"
    );
    assert_eq!(headers["x-plain"], "nothing");
    assert_eq!(notes.ids(), ["stand-in:secret:GH_TOKEN"]);
    assert!(notes.withheld());
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

fn raw_deflate(data: &[u8]) -> Vec<u8> {
    let mut encoder =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

/// `{"echo":"Bearer ghp_realRealReal1111 done"}` compressed with Brotli (Python's `brotli`).
const BROTLI: [u8; 47] = [
    27, 42, 0, 248, 69, 221, 150, 234, 89, 104, 171, 143, 134, 104, 40, 114, 100, 248, 32, 40, 175,
    98, 74, 162, 74, 69, 5, 128, 40, 255, 65, 51, 15, 28, 77, 81, 42, 91, 121, 71, 41, 118, 23, 73,
    90, 225, 3,
];

async fn answer(
    encoding: Option<&str>,
    body: Vec<u8>,
    no_body: bool,
) -> Result<(HeaderMap, Result<Vec<u8>, String>), NotScrubbed> {
    let mut builder = Response::builder().header("content-length", body.len());
    if let Some(encoding) = encoding {
        builder = builder.header("content-encoding", encoding);
    }
    let response = builder
        .body(AnswerBody::Buffered(Some(Bytes::from(body))))
        .unwrap();
    let notes = Arc::new(ScrubNotes::default());
    let scrubbed = scrub_answer(
        &registry(),
        response,
        &host("api.github.com"),
        no_body,
        &notes,
        Duration::from_secs(5),
    )
    .await?;
    let (parts, body) = scrubbed.into_parts();
    let collected = body
        .collect()
        .await
        .map(|all| all.to_bytes().to_vec())
        .map_err(|err| err.to_string());
    Ok((parts.headers, collected))
}

#[tokio::test]
async fn encoded_answers_are_decoded_scrubbed_and_sent_plain() {
    let plain = format!("{{\"echo\":\"Bearer {REAL_GH} done\"}}");
    let expected = format!("{{\"echo\":\"Bearer {GH} done\"}}");
    let zstd = zstd::stream::encode_all(plain.as_bytes(), 3).unwrap();
    for (encoding, body) in [
        ("gzip", gzip(plain.as_bytes())),
        ("x-gzip", gzip(plain.as_bytes())),
        ("deflate", zlib(plain.as_bytes())),
        ("deflate", raw_deflate(plain.as_bytes())),
        ("br", BROTLI.to_vec()),
        ("zstd", zstd),
        ("identity", plain.clone().into_bytes()),
    ] {
        let (headers, body) = answer(Some(encoding), body, false).await.unwrap();
        assert_eq!(text(&body.unwrap()), expected, "{encoding}");
        if encoding == "identity" {
            // A body in hand is scrubbed whole and keeps an exact length.
            assert_eq!(headers["content-length"], expected.len().to_string());
        } else {
            assert!(headers.get("content-encoding").is_none(), "{encoding}");
            assert!(headers.get("content-length").is_none(), "{encoding}");
        }
    }
    // Two gzip members in a row are one body.
    let mut two = gzip(b"first ");
    two.extend(gzip(REAL_GH.as_bytes()));
    let (_, body) = answer(Some("gzip"), two, false).await.unwrap();
    assert_eq!(text(&body.unwrap()), format!("first {GH}"));
}

#[tokio::test]
async fn an_answer_in_an_encoding_puddle_does_not_read_is_refused() {
    for encoding in ["compress", "gzip, br", "dcz", "\u{7f}"] {
        let Ok(value) = HeaderValue::from_str(encoding) else {
            continue;
        };
        let response = Response::builder()
            .header("content-encoding", value)
            .body(AnswerBody::Buffered(Some(Bytes::from_static(b"x"))))
            .unwrap();
        let notes = Arc::new(ScrubNotes::default());
        let refused = scrub_answer(
            &registry(),
            response,
            &host("api.github.com"),
            false,
            &notes,
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(refused.err(), Some(NotScrubbed::Encoding), "{encoding}");
    }
    // An answer without a body has nothing to decode.
    let (headers, _) = answer(Some("compress"), Vec::new(), true).await.unwrap();
    assert_eq!(headers["content-encoding"], "compress");
    // A broken encoding ends the body with an error, never with what could not be read.
    let (_, body) = answer(Some("gzip"), b"not gzip at all".to_vec(), false)
        .await
        .unwrap();
    assert!(body.unwrap_err().contains("could not be decoded"));
}

#[tokio::test]
async fn a_body_in_hand_keeps_an_exact_length_and_one_that_must_be_cut_off_fails() {
    let (headers, body) = answer(None, b"nothing secret".to_vec(), false)
        .await
        .unwrap();
    assert_eq!(body.unwrap(), b"nothing secret");
    assert_eq!(
        headers["content-length"], "14",
        "a body in hand keeps an exact length"
    );
    let blob = STANDARD.encode(format!("x{REAL_GH}tail")).into_bytes();
    let (_, body) = answer(None, blob, false).await.unwrap();
    assert!(body.unwrap_err().contains("cannot replace"));
}

#[test]
fn accept_encoding_is_limited_to_what_puddle_decodes() {
    let limited = |value: &str| {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT_ENCODING, value.parse().unwrap());
        limit_accept_encoding(&mut headers);
        headers
            .get(header::ACCEPT_ENCODING)
            .map(|v| v.to_str().unwrap().to_owned())
    };
    assert_eq!(
        limited("gzip, deflate, br, zstd").as_deref(),
        Some("gzip, deflate, br, zstd")
    );
    assert_eq!(
        limited("dcb, br;q=1.0, compress, *;q=0.1").as_deref(),
        Some("br;q=1.0")
    );
    assert_eq!(limited("compress, *"), None);
    let mut none = HeaderMap::new();
    limit_accept_encoding(&mut none);
    assert!(none.is_empty());
}

fn piece_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        Just(REAL_GH.as_bytes().to_vec()),
        Just(REAL_ODD.as_bytes().to_vec()),
        Just(GH.as_bytes().to_vec()),
        Just(STANDARD.encode(REAL_GH).into_bytes()),
        Just(b"a%2Fb%20c%2Bd%22e%5Cf%3Cg%3E%26%C3%A9%3Dk".to_vec()),
        (1..REAL_GH.len()).prop_map(|n| REAL_GH.as_bytes()[..n].to_vec()),
        (1..REAL_GH.len()).prop_map(|n| REAL_GH.as_bytes()[n..].to_vec()),
        proptest::collection::vec(proptest::char::range(' ', '~'), 0..12)
            .prop_map(|chars| chars.into_iter().collect::<String>().into_bytes()),
    ]
}

proptest! {
    #[test]
    fn a_body_split_anywhere_comes_out_as_it_does_whole(
        pieces in proptest::collection::vec(piece_strategy(), 0..12),
        cuts in proptest::collection::vec(any::<prop::sample::Index>(), 0..6),
    ) {
        let scrubber = scrubber();
        let body: Vec<u8> = pieces.concat();
        let one = whole(&scrubber, &body);
        // Every single split offset.
        for at in 0..=body.len() {
            let (a, b) = body.split_at(at);
            prop_assert_eq!(&stream(&scrubber, &[a, b]).unwrap(), &one);
        }
        // Several random cuts.
        let mut offsets: Vec<usize> = cuts.iter().map(|i| i.index(body.len() + 1)).collect();
        offsets.sort_unstable();
        let mut parts = Vec::new();
        let mut from = 0;
        for at in offsets {
            parts.push(&body[from..at]);
            from = at;
        }
        parts.push(&body[from..]);
        prop_assert_eq!(&stream(&scrubber, &parts).unwrap(), &one);
        // Nothing real is left.
        let out = String::from_utf8_lossy(&one);
        prop_assert!(!out.contains(REAL_GH));
        prop_assert!(!out.contains(REAL_ODD));
        prop_assert!(!out.contains(&STANDARD.encode(REAL_GH)));
    }

    #[test]
    fn random_bytes_never_panic_and_change_nothing_without_a_real_value(
        body in proptest::collection::vec(any::<u8>(), 0..400),
        at in any::<prop::sample::Index>(),
    ) {
        let scrubber = scrubber();
        let at = at.index(body.len() + 1);
        let (a, b) = body.split_at(at);
        if let Ok(out) = stream(&scrubber, &[a, b]) {
            prop_assert_eq!(out, body);
        }
    }
}
