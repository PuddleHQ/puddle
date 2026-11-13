// SPDX-License-Identifier: AGPL-3.0-or-later

use ::http::header::HeaderName;
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

fn scrubber() -> Arc<Scrubber> {
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
fn a_change_to_the_registry_applies_to_the_next_answer() {
    let registry = registry();
    let to = host("api.github.com");
    let first = registry.answer_scrubber(&to).unwrap();
    assert!(
        Arc::ptr_eq(&first, &registry.answer_scrubber(&to).unwrap()),
        "made once"
    );
    let refreshed = "ghp_refreshedRefreshed22";
    registry.set_real(GH, SecretValue::new(refreshed)).unwrap();
    let now = registry.answer_scrubber(&to).unwrap();
    assert_eq!(text(&whole(&now, refreshed.as_bytes())), GH);
    assert!(registry.remove(GH));
    let now = registry.answer_scrubber(&to).unwrap();
    assert_eq!(text(&whole(&now, refreshed.as_bytes())), refreshed);
    assert!(registry.remove(ODD));
    assert!(registry.answer_scrubber(&to).is_none());
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

/// Throughput of the body scrub and the cost of a header scrub, by hand in a release build:
/// `cargo nextest run --release -p puddle-proxy --lib -E 'test(scrub_speed)' --run-ignored only --no-capture`.
#[test]
#[ignore = "a measurement, meaningful only in a release build"]
fn scrub_speed() {
    use std::time::Instant;
    const MIB: usize = 1024 * 1024;
    let text: Vec<u8> = br#"{"id":12345,"name":"package-name","version":"1.2.3","dist":{"shasum":"abcdef0123456789"}},"#
        .iter()
        .copied()
        .cycle()
        .take(16 * MIB)
        .collect();
    let bodies: [(&str, Vec<u8>); 3] = [
        ("x", vec![b'x'; 16 * MIB]),
        ("json", text),
        // Every byte starts a form: the worst case for the held tail.
        ("all-s", vec![b's'; 16 * MIB]),
    ];
    for count in [1_usize, 8, 50] {
        let registry = StandIns::new();
        for n in 0..count {
            let name = format!("S{n}");
            registry
                .insert(entry(
                    StandInOrigin::Secret,
                    &name,
                    &format!("puddle-secret-S{n}-{n:032x}"),
                    &format!("sk-live-{n}-5c1d9e0a47b2a9f0e1d2c3b4"),
                    &["api.github.com"],
                ))
                .unwrap();
        }
        let started = Instant::now();
        let scrubber = registry.answer_scrubber(&host("api.github.com")).unwrap();
        eprintln!(
            "M build registered={count} {:.0} us (once per host until the registry changes)",
            started.elapsed().as_secs_f64() * 1e6
        );
        for (kind, body) in &bodies {
            let pieces: Vec<&[u8]> = body.chunks(16 * 1024).collect();
            let started = Instant::now();
            let out = stream(&scrubber, &pieces).unwrap();
            let took = started.elapsed();
            assert_eq!(out.len(), body.len());
            eprintln!(
                "M body registered={count} patterns={} kind={kind} {:.0} MiB/s",
                scrubber.patterns.len(),
                16.0 / took.as_secs_f64()
            );
        }
        let mut headers = HeaderMap::new();
        for n in 0..17 {
            headers.append(
                HeaderName::from_bytes(format!("x-h{n}").as_bytes()).unwrap(),
                HeaderValue::from_static("some ordinary header value of a typical length here"),
            );
        }
        headers.append(
            "set-cookie",
            HeaderValue::from_str(&"c".repeat(3000)).unwrap(),
        );
        let rounds = 2000;
        let started = Instant::now();
        for _ in 0..rounds {
            let mut copy = headers.clone();
            let scrubber = registry.answer_scrubber(&host("api.github.com")).unwrap();
            scrubber.scrub_headers(&mut copy, &ScrubNotes::default());
        }
        eprintln!(
            "M headers registered={count} {:.1} us per answer (scrubber made already, 18 headers, a 3 KB cookie)",
            started.elapsed().as_secs_f64() * 1e6 / f64::from(rounds)
        );
    }
}

#[test]
fn a_scrubber_shows_names_never_values_and_the_cache_stays_bounded() {
    let registry = StandIns::new();
    registry
        .insert(entry(
            StandInOrigin::Secret,
            "GH_TOKEN",
            GH,
            REAL_GH,
            &["*.example.com"],
        ))
        .unwrap();
    let shown = format!(
        "{:?}",
        registry.answer_scrubber(&host("a.example.com")).unwrap()
    );
    assert!(
        shown.contains("stand-in:secret:GH_TOKEN") && !shown.contains(REAL_GH),
        "{shown}"
    );
    // More hosts than the cache keeps: it starts over and still answers right.
    for n in 0..=MAX_CACHED_SCRUBBERS {
        let to = host(&format!("h{n}.example.com"));
        assert!(registry.answer_scrubber(&to).is_some());
    }
    // The headers of an answer whose body is not read (a WebSocket upgrade).
    let mut headers = HeaderMap::new();
    headers.insert("x-echo", REAL_GH.parse().unwrap());
    let notes = ScrubNotes::default();
    registry.scrub_answer_headers(&mut headers, &host("a.example.com"), &notes);
    assert_eq!(headers["x-echo"], GH);
    assert_eq!(notes.ids(), ["stand-in:secret:GH_TOKEN"]);
}

#[test]
fn json_escapes_control_characters_as_an_encoder_writes_them() {
    assert_eq!(
        text(&json("a\n\r\tb\u{1}", Json::default())),
        "a\\n\\r\\tb\\u0001"
    );
}

#[tokio::test]
async fn an_empty_deflate_body_is_empty_and_a_cut_one_is_an_error() {
    let (_, body) = answer(Some("deflate"), Vec::new(), false).await.unwrap();
    assert_eq!(body.unwrap(), b"");
    let (_, body) = answer(Some("deflate"), vec![0x78], false).await.unwrap();
    assert!(body.unwrap_err().contains("could not be decoded"));
}

#[test]
fn a_held_answer_that_does_not_arrive_whole_is_a_bad_gateway() {
    let refusal = answer_failed_refusal(&host("api.github.com"));
    assert_eq!(refusal.status, "502 Bad Gateway");
    assert_eq!(refusal.message, "api.github.com did not finish its answer");
}

/// A scrubber for one host from `(name, stand-in, real)` rows, all secrets.
fn scrubber_of(rows: &[(&str, &str, &str)]) -> Arc<Scrubber> {
    let registry = StandIns::new();
    for (name, stand_in, real) in rows {
        registry
            .insert(entry(
                StandInOrigin::Secret,
                name,
                stand_in,
                real,
                &["api.github.com"],
            ))
            .unwrap();
    }
    registry.answer_scrubber(&host("api.github.com")).unwrap()
}

#[test]
fn a_scrubber_made_once_is_kept_while_the_cache_has_room() {
    let registry = registry();
    let first = registry.answer_scrubber(&host("api.github.com")).unwrap();
    let _other = registry.answer_scrubber(&host("odd.example.com")).unwrap();
    let again = registry.answer_scrubber(&host("api.github.com")).unwrap();
    assert!(Arc::ptr_eq(&first, &again));
}

#[test]
fn the_first_stand_in_keeps_a_form_two_values_share() {
    let form = || Zeroizing::new(b"shared".to_vec());
    let mut patterns = Vec::new();
    add(&mut patterns, form(), Some(b"one".to_vec()), 0);
    add(&mut patterns, form(), Some(b"two".to_vec()), 1);
    assert_eq!((patterns.len(), patterns[0].id), (1, 0));
    assert_eq!(patterns[0].put.as_deref(), Some(&b"one"[..]));

    let mut cut = Vec::new();
    add(&mut cut, form(), None, 0);
    add(&mut cut, form(), None, 1);
    assert_eq!(cut[0].id, 0);
    // A replaceable form wins over the same bytes that cannot be replaced.
    add(&mut cut, form(), Some(b"two".to_vec()), 1);
    assert_eq!((cut[0].id, cut[0].put.as_deref()), (1, Some(&b"two"[..])));
}

#[test]
fn json_styles_that_escape_one_thing_are_matched() {
    // Python's default: only what is outside ASCII is written as \uXXXX.
    let accent = scrubber_of(&[("A", GH, "na\u{ef}ve/token-1234567")]);
    assert_eq!(
        text(&whole(&accent, b"x na\\u00efve/token-1234567 y")),
        format!("x {GH} y")
    );
    // Go's default: <, > and & as \uXXXX, but not "/". A "/" tells each style from the one that escapes everything.
    let angle = scrubber_of(&[("A", GH, "a<b>&c/token-1234567")]);
    assert_eq!(
        text(&whole(
            &angle,
            b"x a\\u003cb\\u003e\\u0026c/token-1234567 y"
        )),
        format!("x {GH} y")
    );
}

#[test]
fn base64_cores_need_twelve_bytes_and_sit_at_every_alignment() {
    assert_eq!(base64_cores("12345678901").len(), 0);
    assert_eq!(base64_cores("123456789012").len(), 6);
    let real = "puddle-real-value-1234";
    let cores = base64_cores(real);
    for (which, engine) in [&STANDARD_NO_PAD, &URL_SAFE_NO_PAD].into_iter().enumerate() {
        for lead in 0..3_usize {
            // Bytes around the value that are not zero, as in a real document.
            let mut input = vec![0xff_u8; lead];
            input.extend_from_slice(real.as_bytes());
            input.extend_from_slice(&[0xff, 0xff]);
            let encoded = engine.encode(&input).into_bytes();
            let core = &cores[which * 3 + lead];
            // Exactly the characters the value alone decides.
            assert_eq!(
                core.len(),
                8 * (lead + real.len()) / 6 - (8 * lead).div_ceil(6)
            );
            assert!(
                encoded.windows(core.len()).any(|at| at == &core[..]),
                "the core at lead {lead} is not inside the encoding"
            );
        }
    }
}

#[tokio::test]
async fn a_body_is_held_whole_up_to_the_cap_and_refused_over_it() {
    let mut exact = AnswerBody::Buffered(Some(Bytes::from(vec![b'x'; 65_536])));
    let held = hold(&mut exact, Duration::from_secs(5)).await.unwrap();
    assert_eq!(held.len(), 65_536);
    let mut over = AnswerBody::Buffered(Some(Bytes::from(vec![b'x'; 65_537])));
    assert!(hold(&mut over, Duration::from_secs(5)).await.is_err());
}

#[test]
fn a_form_that_is_complete_at_the_end_of_a_piece_is_replaced_at_once() {
    let one = scrubber_of(&[("A", GH, "abc")]);
    let notes = ScrubNotes::default();
    let mut out = Vec::new();
    let used = one.run(b"xxabc", false, &mut out, &notes).unwrap();
    assert_eq!((text(&out), used), (format!("xx{GH}"), 5));
}

#[test]
fn what_follows_a_replaced_form_is_not_held_for_a_form_that_began_inside_it() {
    // "bcd" is the start of "bcde" and sits inside "abc"+"d": after "abc" is replaced,
    // the "d" cannot start a form, so it goes out now.
    let two = scrubber_of(&[("A", GH, "abc"), ("B", ODD, "bcde")]);
    let notes = ScrubNotes::default();
    let mut out = Vec::new();
    let used = two.run(b"abcd", false, &mut out, &notes).unwrap();
    assert_eq!((text(&out), used), (format!("{GH}d"), 4));
}

#[test]
fn nothing_replaced_in_a_whole_input_is_none() {
    let notes = ScrubNotes::default();
    assert!(
        scrubber()
            .scrub_whole(b"nothing here", &notes)
            .unwrap()
            .is_none()
    );
    let changed = scrubber()
        .scrub_whole(REAL_GH.as_bytes(), &notes)
        .unwrap()
        .unwrap();
    assert_eq!(text(&changed), GH);
}

#[tokio::test]
async fn a_raw_deflate_answer_is_not_mistaken_for_zlib_by_half_a_header() {
    // Stored blocks written by hand. First: the low nibble of byte 0 is 8 (a zlib method) but
    // the header check fails. Second: the check passes (0x0117 % 31 == 0) but the nibble is 1.
    let mut nibble = vec![0x08, 0x05, 0x00, 0xfa, 0xff];
    nibble.extend_from_slice(b"hello");
    nibble.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
    let mut check = vec![0x01, 0x17, 0x00, 0xe8, 0xff];
    check.extend_from_slice(b"abcdefghijklmnopqrstuvw");
    for (body, plain) in [(nibble, &b"hello"[..]), (check, b"abcdefghijklmnopqrstuvw")] {
        let (_, got) = answer(Some("deflate"), body, false).await.unwrap();
        assert_eq!(got.unwrap(), plain);
    }
}

/// An answer with `status` from `registry`'s host, the body buffered.
async fn answered(
    registry: &StandIns,
    status: u16,
    encoding: Option<&str>,
    body: Vec<u8>,
) -> Response<AnswerBody> {
    let mut builder = Response::builder()
        .status(status)
        .header("content-length", body.len());
    if let Some(encoding) = encoding {
        builder = builder.header("content-encoding", encoding);
    }
    let response = builder
        .body(AnswerBody::Buffered(Some(Bytes::from(body))))
        .unwrap();
    scrub_answer(
        registry,
        response,
        &host("api.github.com"),
        false,
        &Arc::new(ScrubNotes::default()),
        Duration::from_secs(5),
    )
    .await
    .unwrap_or_else(|_| panic!("not scrubbed"))
}

#[tokio::test]
async fn an_answer_that_has_no_body_by_its_status_is_passed_on_as_it_is() {
    for status in [100, 204, 304] {
        let (_, body) = answered(&registry(), status, None, REAL_GH.as_bytes().to_vec())
            .await
            .into_parts();
        let got = body.collect().await.unwrap().to_bytes();
        assert_eq!(&got[..], REAL_GH.as_bytes(), "status {status}");
    }
}

/// A body that holds a real value inside longer base64, which cannot be replaced: the answer is
/// cut off and goes the streaming way.
fn withheld_body() -> Vec<u8> {
    let mut input = vec![0xff_u8];
    input.extend_from_slice(REAL_LOGIN.as_bytes());
    input.push(0xff);
    STANDARD_NO_PAD.encode(&input).into_bytes()
}

fn same_length_registry() -> StandIns {
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
    registry
}

#[tokio::test]
async fn an_answer_that_streams_keeps_a_length_only_when_it_stays_the_same() {
    for (registry, kept) in [(same_length_registry(), true), (registry(), false)] {
        let body = if kept {
            withheld_body()
        } else {
            {
                let mut input = vec![0xff_u8];
                input.extend_from_slice(REAL_GH.as_bytes());
                input.push(0xff);
                STANDARD_NO_PAD.encode(&input).into_bytes()
            }
        };
        let (parts, _) = answered(&registry, 200, None, body).await.into_parts();
        assert_eq!(parts.headers.contains_key("content-length"), kept);
    }
    // A decoded answer never keeps the length of the encoded one.
    let (parts, _) = answered(&same_length_registry(), 200, Some("gzip"), gzip(b"{}"))
        .await
        .into_parts();
    assert!(!parts.headers.contains_key("content-length"));
    assert!(!parts.headers.contains_key("content-encoding"));
}

#[tokio::test]
async fn a_scrubbed_body_says_what_it_knows_of_its_end_and_size() {
    use http_body_util::BodyExt as _;
    // Not decoded and the same length: the size of what is inside.
    let cut = withheld_body();
    let length = cut.len() as u64;
    let (_, body) = answered(&same_length_registry(), 200, None, cut)
        .await
        .into_parts();
    assert_eq!(body.size_hint().exact(), Some(length));
    // Decoded: nothing is known of the size, even when the lengths are the same.
    let (_, mut body) = answered(
        &same_length_registry(),
        200,
        Some("gzip"),
        gzip(b"{\"a\":1}"),
    )
    .await
    .into_parts();
    assert_eq!(body.size_hint().upper(), None);
    assert!(!body.is_end_stream());
    let mut data = 0;
    while let Some(frame) = body.frame().await {
        let frame = frame.unwrap();
        if let Some(bytes) = frame.data_ref() {
            assert!(!bytes.is_empty(), "no empty frame between the pieces");
            data += 1;
        }
    }
    assert_eq!(data, 1);
    assert!(body.is_end_stream());
}

/// `size` zero bytes, compressed in each coding Puddle reads that the tests can produce.
fn bombs(size: usize) -> Vec<(&'static str, Vec<u8>)> {
    let zeros = vec![0_u8; size];
    vec![
        ("gzip", gzip(&zeros)),
        ("deflate", zlib(&zeros)),
        ("deflate", raw_deflate(&zeros)),
        ("zstd", zstd::stream::encode_all(&zeros[..], 3).unwrap()),
    ]
}

#[test]
fn a_frame_that_inflates_past_the_limit_stops_decoding_there() {
    const LIMIT: usize = 1024 * 1024;
    for (encoding, packed) in bombs(48 * LIMIT) {
        assert!(packed.len() < LIMIT, "{encoding}: {}", packed.len());
        let mut decoder = Decoder::for_encoding(Some(&HeaderValue::from_static(encoding)))
            .unwrap()
            .unwrap();
        let mut out = Vec::new();
        let pushed = decoder.push(&packed, &mut out, LIMIT).unwrap();
        assert_eq!(pushed, Pushed::Full, "{encoding}");
        // The decoder's own write size is all it may run over by.
        assert!(
            (LIMIT..=LIMIT + 256 * 1024).contains(&out.len()),
            "{encoding}: {}",
            out.len()
        );
        // It is spent: nothing more comes out, and the end gives no more either.
        assert_eq!(
            decoder.push(&packed, &mut out, LIMIT).unwrap(),
            Pushed::Full
        );
    }
}

#[test]
fn no_room_left_stops_before_a_piece_that_would_add_nothing() {
    // A gzip header decodes to no bytes, so only the room check can say `Full` for it.
    let packed = gzip(b"payload");
    let mut decoder = Decoder::for_encoding(Some(&HeaderValue::from_static("gzip")))
        .unwrap()
        .unwrap();
    let mut out = Vec::new();
    assert_eq!(
        decoder.push(&packed[..10], &mut out, 0).unwrap(),
        Pushed::Full
    );
    assert_eq!(out, Vec::<u8>::new());
}

#[test]
fn a_frame_under_the_limit_decodes_whole_in_every_coding() {
    let plain = format!("{{\"echo\":\"Bearer {REAL_GH} done\"}}");
    let zstd = zstd::stream::encode_all(plain.as_bytes(), 3).unwrap();
    for (encoding, packed) in [
        ("gzip", gzip(plain.as_bytes())),
        ("deflate", zlib(plain.as_bytes())),
        ("deflate", raw_deflate(plain.as_bytes())),
        ("br", BROTLI.to_vec()),
        ("zstd", zstd),
    ] {
        let mut decoder = Decoder::for_encoding(Some(&HeaderValue::from_static(encoding)))
            .unwrap()
            .unwrap();
        let mut out = Vec::new();
        assert_eq!(
            decoder.push(&packed, &mut out, 1 << 20).unwrap(),
            Pushed::All
        );
        decoder.finish(&mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), plain, "{encoding}");
    }
}

/// The sizes of the data frames of the scrubbed answer, or the error that ended it.
async fn scrubbed_frames(encoding: &str, body: Vec<u8>) -> Result<Vec<usize>, String> {
    let response = answered(&registry(), 200, Some(encoding), body).await;
    let mut body = response.into_body();
    let mut sizes = Vec::new();
    while let Some(frame) = body.frame().await {
        match frame {
            Ok(frame) => {
                if let Ok(data) = frame.into_data() {
                    sizes.push(data.len());
                }
            }
            Err(err) => return Err(err.to_string()),
        }
    }
    Ok(sizes)
}

#[tokio::test]
async fn a_frame_that_inflates_a_lot_reaches_the_guest_in_bounded_pieces() {
    const SIZE: usize = 32 * 1024 * 1024;
    for (encoding, packed) in bombs(SIZE).into_iter().filter(|(c, _)| *c != "zstd") {
        let sizes = scrubbed_frames(encoding, packed).await.unwrap();
        assert_eq!(
            sizes.iter().sum::<usize>(),
            SIZE,
            "{encoding}: nothing is lost"
        );
        let largest = sizes.iter().max().copied().unwrap_or_default();
        assert!(
            largest <= MAX_DECODED_STEP,
            "{encoding}: a frame of {largest} bytes"
        );
        assert!(sizes.len() > 1, "{encoding}: {} frames", sizes.len());
    }
}

#[tokio::test]
async fn a_step_that_inflates_past_the_step_limit_ends_the_answer_with_an_error() {
    let zstd = zstd::stream::encode_all(&vec![0_u8; 3 * MAX_DECODED_STEP][..], 3).unwrap();
    assert!(
        zstd.len() <= DECODE_STEP,
        "one step of the input: {}",
        zstd.len()
    );
    let err = scrubbed_frames("zstd", zstd).await.unwrap_err();
    assert!(err.contains("inflates too far"), "{err}");
}

#[test]
fn the_limit_counts_what_this_call_adds_not_what_the_caller_already_holds() {
    const LIMIT: usize = 1024 * 1024;
    for (encoding, packed) in bombs(8 * LIMIT) {
        let mut decoder = Decoder::for_encoding(Some(&HeaderValue::from_static(encoding)))
            .unwrap()
            .unwrap();
        let mut out = vec![1_u8; 4 * LIMIT];
        assert_eq!(
            decoder.push(&packed, &mut out, LIMIT).unwrap(),
            Pushed::Full,
            "{encoding}"
        );
        let added = out.len() - 4 * LIMIT;
        assert!(
            (LIMIT..=LIMIT + 256 * 1024).contains(&added),
            "{encoding}: {added}"
        );
    }
}

#[test]
fn a_body_that_stops_short_fails_at_its_end_in_the_codings_that_can_tell() {
    let plain = vec![b'x'; 50_000];
    for (encoding, whole) in [
        ("gzip", gzip(&plain)),
        ("deflate", zlib(&plain)),
        ("deflate", raw_deflate(&plain)),
    ] {
        let mut decoder = Decoder::for_encoding(Some(&HeaderValue::from_static(encoding)))
            .unwrap()
            .unwrap();
        let mut out = Vec::new();
        let cut = whole.len() - 4;
        decoder.push(&whole[..cut], &mut out, 1 << 20).unwrap();
        assert!(decoder.finish(&mut out).is_err(), "{encoding} cut short");
        let mut decoder = Decoder::for_encoding(Some(&HeaderValue::from_static(encoding)))
            .unwrap()
            .unwrap();
        let mut out = Vec::new();
        decoder.push(&whole, &mut out, 1 << 20).unwrap();
        decoder.finish(&mut out).unwrap();
        assert_eq!(out, plain, "{encoding}");
    }
}

#[tokio::test]
async fn a_plain_body_is_passed_on_in_the_frames_it_came_in() {
    let body = ScrubBody {
        inner: AnswerBody::Buffered(Some(Bytes::from(vec![b'a'; 100_000]))),
        scrubber: scrubber(),
        queued: Bytes::new(),
        decoder: None,
        held: Zeroizing::new(Vec::new()),
        notes: Arc::new(ScrubNotes::default()),
        trailers: None,
        ended: false,
    };
    let mut body = body;
    let mut sizes = Vec::new();
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame.unwrap().into_data() {
            sizes.push(data.len());
        }
    }
    // The scrubber keeps the last byte back until the end; nothing else splits the body.
    assert_eq!(sizes, [99_999, 1]);
}
