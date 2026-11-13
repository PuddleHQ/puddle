// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::Write as _;

use ::http::header::{CONTENT_ENCODING, CONTENT_TYPE};
use http_body_util::BodyExt as _;
use proptest::prelude::*;
use puddle_test_kit::{TokenKind, jwt, token, with_broken_checksum};
use puddle_types::{CollectingSink, Event};

use super::*;
use crate::terminate::ForeignCredentialWarning;
use crate::terminate::inject::SecretValue;
use crate::terminate::set::TerminationSet;
use crate::terminate::stand_in::{StandIn, StandInOrigin};

const NO_ONE: fn(&[u8]) -> bool = |_| false;

fn seen(chunks: &[&[u8]], holds: &dyn Fn(&[u8]) -> bool) -> Vec<UnknownTokenKind> {
    let mut scanner = TokenScanner::default();
    let mut all = Vec::new();
    for chunk in chunks {
        all.extend(scanner.feed(chunk, false, holds));
    }
    all.extend(scanner.feed(&[], true, holds));
    all
}

fn whole(text: &str) -> Vec<UnknownTokenKind> {
    seen(&[text.as_bytes()], &NO_ONE)
}

fn claims() -> &'static str {
    r#"{"sub":"someone","aud":"somewhere"}"#
}

/// Every kind, built at run time, with the kind it must be read as.
fn every_kind() -> Vec<(String, UnknownTokenKind)> {
    vec![
        (
            token(TokenKind::GithubPersonal, "a"),
            UnknownTokenKind::GithubToken,
        ),
        (
            token(TokenKind::GithubOauth, "b"),
            UnknownTokenKind::GithubToken,
        ),
        (
            token(TokenKind::GithubRefresh, "c"),
            UnknownTokenKind::GithubToken,
        ),
        (
            token(TokenKind::GithubFineGrained, "d"),
            UnknownTokenKind::GithubFineGrainedToken,
        ),
        (token(TokenKind::Gitlab, "e"), UnknownTokenKind::GitlabToken),
        (token(TokenKind::Npm, "f"), UnknownTokenKind::NpmToken),
        (
            token(TokenKind::AzureDevops, "g"),
            UnknownTokenKind::AzureDevopsToken,
        ),
        (
            token(TokenKind::Anthropic, "h"),
            UnknownTokenKind::AnthropicKey,
        ),
        (
            token(TokenKind::OpenaiProject, "i"),
            UnknownTokenKind::OpenaiKey,
        ),
    ]
}

#[test]
fn each_known_kind_is_found_in_json_a_form_and_plain_text() {
    for (made, kind) in every_kind() {
        for body in [
            format!(r#"{{"access_token":"{made}","token_type":"bearer"}}"#),
            format!("access_token={made}&scope=repo"),
            format!("your token is {made}\n"),
            made.clone(),
        ] {
            assert_eq!(whole(&body), [kind], "{kind:?} in {body:?}");
        }
    }
}

#[test]
fn a_kind_is_told_once_however_often_it_comes() {
    let (made, kind) = every_kind().remove(0);
    let body = format!("{made} {made} {made}");
    assert_eq!(whole(&body), [kind]);
    let other = token(TokenKind::GithubOauth, "other");
    assert_eq!(whole(&format!("{made} {other}")), [kind]);
}

#[test]
fn a_token_that_fails_its_own_shape_is_not_found() {
    let github = token(TokenKind::GithubPersonal, "x");
    let npm = token(TokenKind::Npm, "x");
    let fine = token(TokenKind::GithubFineGrained, "x");
    let azure = token(TokenKind::AzureDevops, "x");
    let anthropic = token(TokenKind::Anthropic, "x");
    let openai = token(TokenKind::OpenaiProject, "x");
    let cases = [
        // The checksum is wrong.
        with_broken_checksum(&github),
        with_broken_checksum(&npm),
        // One character short or long.
        fine.get(..fine.len() - 1).unwrap().to_owned(),
        format!("{fine}a"),
        azure.get(..azure.len() - 1).unwrap().to_owned(),
        format!("{azure}a"),
        // The mark is not where it must be.
        azure.replace("AZDO", "AZDX"),
        // A body too short to be a key.
        "sk-ant-api03-short".to_owned(),
        "sk-proj-short".to_owned(),
        "glpat-short".to_owned(),
        // A prefix and nothing else.
        "ghp_".to_owned(),
        "npm_".to_owned(),
        // A body with a dot is not a key.
        format!("{}.x", anthropic.get(..anthropic.len() - 2).unwrap()),
        format!("{}.x", openai.get(..openai.len() - 2).unwrap()),
        // Not a token at all.
        "the quick brown fox".to_owned(),
        String::new(),
    ];
    for body in cases {
        assert_eq!(whole(&body), [], "{body:?}");
    }
}

#[test]
fn a_token_inside_a_longer_run_of_token_characters_is_not_found() {
    let made = token(TokenKind::GithubPersonal, "run");
    for body in [
        format!("x{made}"),
        format!("{made}x"),
        format!("{made}.more"),
        format!("a-b_{made}"),
    ] {
        assert_eq!(whole(&body), [], "{body:?}");
    }
    // Quotes, spaces, commas, `=` and `:` end a run.
    for body in [
        format!("\"{made}\""),
        format!("a,{made},b"),
        format!("k={made}"),
        format!("k:{made}"),
    ] {
        assert_eq!(whole(&body), [UnknownTokenKind::GithubToken], "{body:?}");
    }
}

#[test]
fn a_jwt_alone_says_nothing_and_with_a_refresh_token_field_it_does() {
    let signed = jwt("s", claims());
    assert_eq!(whole(&signed), []);
    assert_eq!(whole(&format!(r#"{{"id_token":"{signed}"}}"#)), []);
    assert_eq!(whole(r#"{"refresh_token":"abc123"}"#), []);
    for body in [
        format!(r#"{{"access_token":"{signed}","refresh_token":"abc123"}}"#),
        format!(r#"{{"refresh_token":"abc123","access_token":"{signed}"}}"#),
        format!(r#"{{ "refresh_token" : "abc123" , "id_token" : "{signed}" }}"#),
        format!("access_token={signed}&refresh_token=abc123"),
        format!(r#"{{\"refresh_token\":\"abc123\",\"t\":\"{signed}\"}}"#),
    ] {
        assert_eq!(
            whole(&body),
            [UnknownTokenKind::JwtWithRefreshToken],
            "{body}"
        );
    }
}

#[test]
fn a_jwt_must_read_as_one() {
    let signed = jwt("s", claims());
    let parts: Vec<&str> = signed.split('.').collect();
    let refresh = r#""refresh_token":"abc123""#;
    for odd in [
        // Two segments, four segments, an empty signature, a header that is not an object.
        format!("{}.{}", parts[0], parts[1]),
        format!("{signed}.extra"),
        format!("{}.{}.", parts[0], parts[1]),
        "eyJ.e30.sig".to_owned(),
        // A header whose base64 cannot be (five characters).
        format!("eyJhY.{}.{}", parts[1], parts[2]),
    ] {
        assert_eq!(whole(&format!("{odd} {refresh}")), [], "{odd}");
    }
}

#[test]
fn a_refresh_token_field_without_a_value_does_not_pair() {
    let signed = jwt("s", claims());
    for field in [
        r#""refresh_token":"""#,
        r#""refresh_token":null"#,
        r#""refresh_token""#,
        "refresh_token",
        r#""refresh_token":      "x""#,
        r#""refresh_token_expires_in":3600"#,
        r#""xrefresh_token":"abc""#,
    ] {
        assert_eq!(whole(&format!("{signed} {field}")), [], "{field}");
    }
}

#[test]
fn a_refresh_token_field_that_holds_a_stand_in_does_not_pair() {
    let signed = jwt("s", claims());
    let stand_in = token(TokenKind::Gitlab, "mine");
    let holds = |run: &[u8]| run == stand_in.as_bytes();
    let body = format!(r#"{{"id_token":"{signed}","refresh_token":"{stand_in}"}}"#);
    assert_eq!(seen(&[body.as_bytes()], &holds), []);
    // The same field with a value Puddle does not hold does.
    let other = token(TokenKind::Gitlab, "other");
    let body = format!(r#"{{"id_token":"{signed}","refresh_token":"{other}"}}"#);
    assert_eq!(
        seen(&[body.as_bytes()], &holds),
        [
            UnknownTokenKind::GitlabToken,
            UnknownTokenKind::JwtWithRefreshToken
        ]
    );
}

#[test]
fn a_token_puddle_holds_is_never_found() {
    for (made, _) in every_kind() {
        let holds = |run: &[u8]| run == made.as_bytes();
        let body = format!(r#"{{"access_token":"{made}"}}"#);
        assert_eq!(seen(&[body.as_bytes()], &holds), [], "{made}");
    }
}

#[test]
fn a_body_cut_anywhere_gives_what_it_gives_whole() {
    let signed = jwt("s", claims());
    let body = format!(
        r#"{{"a":"{}","b":"{}","c":{{"id_token":"{signed}","refresh_token":"abc123"}},"d":"{}"}}"#,
        token(TokenKind::GithubPersonal, "1"),
        token(TokenKind::Anthropic, "2"),
        token(TokenKind::AzureDevops, "3"),
    );
    let one = seen(&[body.as_bytes()], &NO_ONE);
    assert_eq!(one.len(), 4, "{one:?}");
    let sorted = |mut kinds: Vec<UnknownTokenKind>| {
        kinds.sort_by_key(|kind| kind.as_str());
        kinds
    };
    for at in 0..=body.len() {
        let (a, b) = body.as_bytes().split_at(at);
        assert_eq!(
            sorted(seen(&[a, b], &NO_ONE)),
            sorted(one.clone()),
            "cut at {at}"
        );
    }
    // And one byte at a time.
    let bytes: Vec<&[u8]> = body.as_bytes().chunks(1).collect();
    assert_eq!(sorted(seen(&bytes, &NO_ONE)), sorted(one));
}

#[test]
fn a_run_too_long_to_be_a_token_is_skipped_across_pieces_without_a_find_in_its_tail() {
    let made = token(TokenKind::GithubPersonal, "tail");
    let long = "a".repeat(MAX_RUN + 10);
    let a = long.as_bytes();
    // The long run ends in a token's text: it is still one run.
    let b = format!("{made} ");
    assert_eq!(seen(&[a, b.as_bytes()], &NO_ONE), []);
    assert_eq!(seen(&[a, a, b.as_bytes()], &NO_ONE), []);
    // After a gap the next run is read again.
    let c = format!(" {made} ");
    assert_eq!(
        seen(&[a, c.as_bytes()], &NO_ONE),
        [UnknownTokenKind::GithubToken]
    );
}

#[test]
fn the_scanner_keeps_at_most_one_run_between_pieces() {
    let mut scanner = TokenScanner::default();
    let piece = "word ".repeat(1000) + &"b".repeat(100);
    for _ in 0..4 {
        let _ = scanner.feed(piece.as_bytes(), false, &NO_ONE);
        assert!(scanner.carry.len() <= MAX_RUN, "{}", scanner.carry.len());
    }
}

#[test]
fn nothing_past_the_cap_is_read() {
    let made = token(TokenKind::GithubPersonal, "cap");
    let filler = vec![b' '; MAX_SCANNED];
    let mut scanner = TokenScanner::default();
    assert_eq!(scanner.feed(&filler, false, &NO_ONE), []);
    assert_eq!(scanner.feed(made.as_bytes(), false, &NO_ONE), []);
    assert_eq!(scanner.feed(&[], true, &NO_ONE), []);
    // Just inside it is.
    let mut scanner = TokenScanner::default();
    let lead = vec![b' '; MAX_SCANNED - made.len()];
    assert_eq!(scanner.feed(&lead, false, &NO_ONE), []);
    assert_eq!(
        scanner.feed(made.as_bytes(), false, &NO_ONE),
        [UnknownTokenKind::GithubToken]
    );
}

fn piece_strategy() -> impl Strategy<Value = Vec<u8>> {
    let signed = jwt("p", claims());
    let samples: Vec<String> = every_kind()
        .into_iter()
        .map(|(made, _)| made)
        .chain([
            signed,
            "refresh_token".to_owned(),
            r#""refresh_token":"#.to_owned(),
            r#""refresh_token": ""#.to_owned(),
            "xyz".to_owned(),
            "\"".to_owned(),
            " ".to_owned(),
            ".".to_owned(),
            "-".to_owned(),
            "_".to_owned(),
            ",".to_owned(),
            "\n".to_owned(),
        ])
        .collect();
    prop_oneof![
        proptest::sample::select(samples).prop_map(String::into_bytes),
        proptest::collection::vec(any::<u8>(), 0..24),
    ]
}

fn as_set(kinds: Vec<UnknownTokenKind>) -> Vec<&'static str> {
    let mut names: Vec<&str> = kinds.into_iter().map(UnknownTokenKind::as_str).collect();
    names.sort_unstable();
    names
}

proptest! {
    #[test]
    fn a_body_split_anywhere_is_read_as_it_is_whole(
        pieces in proptest::collection::vec(piece_strategy(), 0..14),
        cuts in proptest::collection::vec(any::<prop::sample::Index>(), 0..6),
    ) {
        let body: Vec<u8> = pieces.concat();
        let one = as_set(seen(&[&body], &NO_ONE));
        for at in 0..=body.len() {
            let (a, b) = body.split_at(at);
            prop_assert_eq!(&as_set(seen(&[a, b], &NO_ONE)), &one);
        }
        let mut offsets: Vec<usize> = cuts.iter().map(|c| c.index(body.len() + 1)).collect();
        offsets.sort_unstable();
        let mut parts: Vec<&[u8]> = Vec::new();
        let mut from = 0;
        for at in offsets {
            parts.push(&body[from..at]);
            from = at;
        }
        parts.push(&body[from..]);
        prop_assert_eq!(&as_set(seen(&parts, &NO_ONE)), &one);
    }

    #[test]
    fn any_bytes_are_read_without_a_fault_and_find_each_kind_at_most_once(
        data in proptest::collection::vec(any::<u8>(), 0..2048),
    ) {
        let kinds = seen(&[&data], &NO_ONE);
        let mut names = as_set(kinds.clone());
        names.dedup();
        prop_assert_eq!(names.len(), kinds.len());
    }

    #[test]
    fn a_token_is_found_wherever_it_sits_between_separators(
        before in "[ \"',:;=\n]{0,8}[a-z ]{0,20}[ \"',:;=\n]",
        after in "[ \"',:;=\n][a-z ]{0,20}[ \"',:;=\n]{0,8}",
        pick in 0usize..9,
    ) {
        let (made, kind) = every_kind().remove(pick);
        let body = format!("{before}{made}{after}");
        prop_assert_eq!(seen(&[body.as_bytes()], &NO_ONE), vec![kind]);
    }
}

// The body and its report.

fn host() -> Host {
    Host::parse_normalised("login.example.invalid").unwrap()
}

fn workspace() -> WorkspaceId {
    WorkspaceId::new("box").unwrap()
}

struct Rig {
    sink: Arc<CollectingSink>,
    report: Arc<AnswerReport>,
    stand_ins: Arc<StandIns>,
}

fn rig(on: bool) -> Rig {
    let sink = Arc::new(CollectingSink::default());
    let report = Arc::new(AnswerReport::new(
        Arc::new(ForeignCredentialWarning::new(move || on)),
        Arc::clone(&sink) as Arc<dyn EventSink>,
        workspace(),
        host(),
    ));
    Rig {
        sink,
        report,
        stand_ins: Arc::new(StandIns::new()),
    }
}

fn answer(
    content_type: Option<&str>,
    encoding: Option<&str>,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Response<AnswerBody> {
    let mut builder = Response::builder().status(200);
    if let Some(value) = content_type {
        builder = builder.header(CONTENT_TYPE, value);
    }
    if let Some(value) = encoding {
        builder = builder.header(CONTENT_ENCODING, value);
    }
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder
        .body(AnswerBody::Buffered(Some(Bytes::from(body))))
        .unwrap()
}

async fn passed(response: Response<AnswerBody>) -> (::http::HeaderMap, Vec<u8>) {
    let (parts, body) = response.into_parts();
    (
        parts.headers,
        body.collect().await.unwrap().to_bytes().to_vec(),
    )
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

#[tokio::test]
async fn an_answer_with_a_token_goes_through_unchanged_and_the_host_and_kind_are_told() {
    let rig = rig(true);
    let made = token(TokenKind::GithubPersonal, "body");
    let body = format!(r#"{{"access_token":"{made}"}}"#).into_bytes();
    let response = answer(
        Some("application/json"),
        None,
        &[("x-a", "b")],
        body.clone(),
    );
    let response = scan_answer(&rig.stand_ins, response, false, &rig.report);
    let (headers, got) = passed(response).await;
    assert_eq!(got, body);
    assert_eq!(headers["x-a"], "b");
    assert_eq!(rig.report.kinds(), ["github_token"]);
    let events = rig.sink.events();
    assert_eq!(
        events,
        [Event::UnknownTokenInAnswer {
            workspace: workspace(),
            host: "login.example.invalid".to_owned(),
            kind: UnknownTokenKind::GithubToken,
        }]
    );
    // Neither the notice nor the record says anything of the value.
    let said = format!("{events:?} {:?}", rig.report.kinds());
    assert!(
        !said.contains(&made) && !said.contains(&made[4..]),
        "{said}"
    );
}

#[tokio::test]
async fn a_token_in_a_header_is_found_and_the_notice_comes_once_for_a_host_and_kind() {
    let rig = rig(true);
    let made = token(TokenKind::Npm, "header");
    for _ in 0..3 {
        let response = answer(
            Some("text/plain"),
            None,
            &[("set-cookie", &format!("session={made}; Path=/"))],
            b"ok".to_vec(),
        );
        let (_, got) = passed(scan_answer(&rig.stand_ins, response, false, &rig.report)).await;
        assert_eq!(got, b"ok");
    }
    assert_eq!(rig.report.kinds(), ["npm_token"]);
    assert_eq!(rig.sink.events().len(), 1);
}

#[tokio::test]
async fn a_warning_that_is_off_finds_nothing_to_tell() {
    let rig = rig(false);
    let made = token(TokenKind::Gitlab, "off");
    let response = answer(None, None, &[], made.into_bytes());
    let _ = passed(scan_answer(&rig.stand_ins, response, false, &rig.report)).await;
    assert_eq!(rig.report.kinds(), Vec::<String>::new());
    assert_eq!(rig.sink.events(), []);
}

#[tokio::test]
async fn an_encoded_body_is_read_decoded_and_passes_as_it_came() {
    let made = token(TokenKind::AzureDevops, "gz");
    let plain = format!(r#"{{"token":"{made}"}}"#);
    let packed = gzip(plain.as_bytes());
    let rig = rig(true);
    let response = answer(Some("application/json"), Some("gzip"), &[], packed.clone());
    let (headers, got) = passed(scan_answer(&rig.stand_ins, response, false, &rig.report)).await;
    assert_eq!(got, packed);
    assert_eq!(headers[CONTENT_ENCODING], "gzip");
    assert_eq!(rig.report.kinds(), ["azure_devops_token"]);
}

#[tokio::test]
async fn a_body_that_cannot_be_decoded_is_passed_and_what_decoded_is_still_read() {
    let rig = rig(true);
    let response = answer(None, Some("gzip"), &[], b"not gzip at all".to_vec());
    let (_, got) = passed(scan_answer(&rig.stand_ins, response, false, &rig.report)).await;
    assert_eq!(got, b"not gzip at all");
    assert_eq!(rig.report.kinds(), Vec::<String>::new());
    // An encoding Puddle does not read: passed, not read, its headers still are.
    let made = token(TokenKind::OpenaiProject, "enc");
    let response = answer(
        Some("text/plain"),
        Some("compress"),
        &[("x-leak", &made)],
        made.clone().into_bytes(),
    );
    let _ = passed(scan_answer(&rig.stand_ins, response, false, &rig.report)).await;
    assert_eq!(rig.report.kinds(), ["openai_key"]);
}

#[tokio::test]
async fn a_body_that_is_not_text_is_not_read_but_its_headers_are() {
    let made = token(TokenKind::Anthropic, "bin");
    let rig = rig(true);
    let response = answer(
        Some("application/octet-stream"),
        None,
        &[],
        made.clone().into_bytes(),
    );
    let _ = passed(scan_answer(&rig.stand_ins, response, false, &rig.report)).await;
    assert_eq!(rig.report.kinds(), Vec::<String>::new());
    let response = answer(
        Some("application/octet-stream"),
        None,
        &[("x-key", &made)],
        Vec::new(),
    );
    let _ = passed(scan_answer(&rig.stand_ins, response, false, &rig.report)).await;
    assert_eq!(rig.report.kinds(), ["anthropic_key"]);
}

#[tokio::test]
async fn a_head_answer_and_the_statuses_without_a_body_are_not_read() {
    let made = token(TokenKind::GithubPersonal, "head");
    let rig = rig(true);
    let response = answer(None, None, &[], made.clone().into_bytes());
    let _ = passed(scan_answer(&rig.stand_ins, response, true, &rig.report)).await;
    for status in [204, 304, 100] {
        let response = Response::builder()
            .status(status)
            .body(AnswerBody::Buffered(Some(Bytes::from(made.clone()))))
            .unwrap();
        let _ = passed(scan_answer(&rig.stand_ins, response, false, &rig.report)).await;
    }
    assert_eq!(rig.report.kinds(), Vec::<String>::new());
}

#[tokio::test]
async fn a_token_the_registry_holds_is_its_own_whether_stand_in_or_real_value() {
    let stand_in = token(TokenKind::Gitlab, "held");
    let real = token(TokenKind::Npm, "real");
    let rig = rig(true);
    rig.stand_ins
        .insert(
            StandIn::new(
                StandInOrigin::Secret,
                "HELD",
                &stand_in,
                SecretValue::new(&real),
                TerminationSet::parse(["login.example.invalid"]).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let body = format!("{stand_in} {real}");
    let response = answer(None, None, &[], body.into_bytes());
    let _ = passed(scan_answer(&rig.stand_ins, response, false, &rig.report)).await;
    assert_eq!(rig.report.kinds(), Vec::<String>::new());
    // A token it does not hold, in the same answer, is found.
    let other = token(TokenKind::GithubPersonal, "other");
    let response = answer(None, None, &[], format!("{stand_in} {other}").into_bytes());
    let _ = passed(scan_answer(&rig.stand_ins, response, false, &rig.report)).await;
    assert_eq!(rig.report.kinds(), ["github_token"]);
}

#[test]
fn only_text_like_types_are_read() {
    let read =
        |text: Option<&str>| is_text_type(text.map(|t| HeaderValue::from_str(t).unwrap()).as_ref());
    for yes in [
        None,
        Some("text/html; charset=utf-8"),
        Some("TEXT/PLAIN"),
        Some("application/json"),
        Some("application/vnd.api+json"),
        Some("application/xml"),
        Some("application/atom+xml"),
        Some("application/javascript"),
        Some("application/x-www-form-urlencoded"),
    ] {
        assert!(read(yes), "{yes:?}");
    }
    for no in [
        "application/octet-stream",
        "image/png",
        "application/x-git-upload-pack-result",
        "application/zip",
        "",
    ] {
        assert!(!read(Some(no)), "{no:?}");
    }
    assert!(!is_text_type(Some(
        &HeaderValue::from_bytes(b"\xff").unwrap()
    )));
}

/// A JWT-shaped run of exactly `len` bytes.
fn jwt_of_length(len: usize) -> String {
    let head = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";
    let signature = "s".repeat(43);
    let claims = "e".repeat(len - head.len() - signature.len() - 2);
    format!("{head}.{claims}.{signature}")
}

#[test]
fn a_run_of_the_longest_length_is_read_and_one_byte_more_is_not() {
    let refresh = r#" {"refresh_token":"x"}"#;
    for (len, found) in [(MAX_RUN, true), (MAX_RUN + 1, false)] {
        let signed = jwt_of_length(len);
        assert_eq!(signed.len(), len);
        let expected: &[UnknownTokenKind] = if found {
            &[UnknownTokenKind::JwtWithRefreshToken]
        } else {
            &[]
        };
        // Whole, with the run ending inside the text.
        let whole = format!("{signed}{refresh}");
        assert_eq!(seen(&[whole.as_bytes()], &NO_ONE), expected, "{len} whole");
        // In pieces, with the run ending a piece (it may still grow).
        assert_eq!(
            seen(&[signed.as_bytes(), refresh.as_bytes()], &NO_ONE),
            expected,
            "{len} in pieces"
        );
        // And the run ending the answer.
        let before = format!(r#"{{"refresh_token":"x"}} {signed}"#);
        assert_eq!(seen(&[before.as_bytes()], &NO_ONE), expected, "{len} last");
    }
}

#[test]
fn a_skipped_run_ends_at_the_first_byte_that_is_not_part_of_it() {
    let made = token(TokenKind::GithubPersonal, "skip");
    let long = "a".repeat(MAX_RUN + 10);
    let (a, b) = (long.as_bytes(), b"x ");
    let c = made.as_bytes();
    assert_eq!(
        seen(&[a, b, c], &NO_ONE),
        [UnknownTokenKind::GithubToken],
        "the space ends the skipped run, so the next piece is read"
    );
    let spaced = format!(" {made}");
    assert_eq!(
        seen(&[a, b"x", spaced.as_bytes()], &NO_ONE),
        [UnknownTokenKind::GithubToken]
    );
    // A piece that ends the skipped run and starts a token after a gap.
    let after = format!("x {made} ");
    assert_eq!(
        seen(&[a, after.as_bytes()], &NO_ONE),
        [UnknownTokenKind::GithubToken]
    );
    // The skipped run goes on over a piece that is all run bytes, and the text after it is read.
    assert_eq!(
        seen(&[a, a, b"x ", c, b" "], &NO_ONE),
        [UnknownTokenKind::GithubToken]
    );
}

#[test]
fn up_to_six_bytes_may_lie_between_a_field_name_and_its_value() {
    let signed = jwt("gap", claims());
    for (gap, pairs) in [(5, true), (6, true), (7, false)] {
        let spaces = " ".repeat(gap);
        let body = format!("{signed} refresh_token{spaces}:{spaces}abc");
        let expected: &[UnknownTokenKind] = if pairs {
            &[UnknownTokenKind::JwtWithRefreshToken]
        } else {
            &[]
        };
        assert_eq!(seen(&[body.as_bytes()], &NO_ONE), expected, "gap {gap}");
    }
}

#[tokio::test]
async fn the_scanned_body_ends_when_the_body_does() {
    let rig = rig(true);
    let response = answer(None, None, &[], b"hello".to_vec());
    let mut body = scan_answer(&rig.stand_ins, response, false, &rig.report).into_body();
    assert!(!body.is_end_stream());
    while body.frame().await.is_some() {}
    assert!(body.is_end_stream());
}

#[test]
fn a_compressed_frame_is_decoded_only_up_to_the_cap_and_the_decoder_is_then_dropped() {
    let late = token(TokenKind::GithubPersonal, "late");
    let early = token(TokenKind::Npm, "early");
    let mut plain = vec![b' '; 40 * MAX_SCANNED];
    plain.extend_from_slice(late.as_bytes());
    let mut first = early.clone().into_bytes();
    first.extend_from_slice(&plain);
    let rig = rig(true);
    for (body, kinds) in [(plain, Vec::<&str>::new()), (first, vec!["npm_token"])] {
        let response = answer(None, Some("gzip"), &[], Vec::new());
        let AnswerBody::Scanned(mut scan) =
            scan_answer(&rig.stand_ins, response, false, &rig.report).into_body()
        else {
            panic!("a text body is scanned");
        };
        scan.read(Some(&gzip(&body)));
        assert!(scan.done && scan.decoder.is_none(), "reading stopped");
        // Later frames are not looked at, whatever they hold.
        scan.read(Some(&gzip(late.as_bytes())));
        scan.read(None);
        assert_eq!(rig.report.kinds(), kinds);
    }
}

#[test]
fn a_plain_body_past_the_cap_is_neither_copied_nor_read() {
    let late = token(TokenKind::GithubPersonal, "plain-late");
    let rig = rig(true);
    let response = answer(None, None, &[], Vec::new());
    let AnswerBody::Scanned(mut scan) =
        scan_answer(&rig.stand_ins, response, false, &rig.report).into_body()
    else {
        panic!("a text body is scanned");
    };
    scan.read(Some(&vec![b' '; MAX_SCANNED]));
    assert!(scan.done, "the cap ends the reading");
    scan.read(Some(late.as_bytes()));
    assert_eq!(rig.report.kinds(), Vec::<String>::new());
}

#[test]
fn a_gzip_body_read_in_pieces_is_read_to_its_end() {
    let made = token(TokenKind::GithubPersonal, "pieces");
    let plain = format!("{} {made} ", "filler ".repeat(2000));
    let packed = gzip(plain.as_bytes());
    let rig = rig(true);
    let response = answer(None, Some("gzip"), &[], Vec::new());
    let AnswerBody::Scanned(mut scan) =
        scan_answer(&rig.stand_ins, response, false, &rig.report).into_body()
    else {
        panic!("a text body is scanned");
    };
    let (first, rest) = packed.split_at(packed.len() / 2);
    scan.read(Some(first));
    assert!(!scan.done && scan.decoder.is_some(), "the body goes on");
    scan.read(Some(rest));
    assert!(!scan.done, "no end yet");
    scan.read(None);
    assert!(scan.done && scan.decoder.is_none());
    assert_eq!(rig.report.kinds(), ["github_token"]);
}
