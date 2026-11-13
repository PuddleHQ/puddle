// SPDX-License-Identifier: AGPL-3.0-or-later
use ::http::header::{AUTHORIZATION, HeaderName, HeaderValue};

use puddle_test_kit::{TokenKind, token, with_broken_checksum, with_checksum};

use super::*;

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.append(
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
    }
    map
}

fn kinds(pairs: &[(&str, &str)], git: bool) -> Vec<ForeignCredentialKind> {
    kinds_in_headers(&headers(pairs), &[], git, &|_| false)
}

const NONE: [ForeignCredentialKind; 0] = [];

const ENTROPY: &str = "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5";

#[test]
fn a_github_token_with_a_valid_checksum_is_named_on_any_path() {
    for kind in TokenKind::GITHUB_CHECKSUMMED {
        let prefix = kind.prefix();
        let token = with_checksum(prefix, ENTROPY);
        for git in [false, true] {
            assert_eq!(
                kinds(&[("authorization", &format!("Bearer {token}"))], git),
                [ForeignCredentialKind::GithubToken],
                "{prefix} git={git}"
            );
        }
    }
}

#[test]
fn a_github_token_with_a_wrong_checksum_is_not_named_here() {
    let token = with_broken_checksum(&with_checksum("ghp_", ENTROPY));
    assert_eq!(
        kinds(&[("authorization", &format!("token {token}"))], false),
        NONE
    );
}

#[test]
fn a_fine_grained_github_token_is_read_by_its_shape() {
    let token = token(TokenKind::GithubFineGrained, "fine");
    assert_eq!(
        kinds(&[("authorization", &format!("Bearer {token}"))], false),
        [ForeignCredentialKind::GithubToken]
    );
    let short = token.get(..token.len() - 1).unwrap().to_owned();
    assert_eq!(
        kinds(&[("authorization", &format!("Bearer {short}"))], false),
        NONE
    );
}

#[test]
fn an_npm_token_with_a_valid_checksum_is_named() {
    let token = with_checksum("npm_", ENTROPY);
    assert_eq!(
        kinds(&[("authorization", &format!("Bearer {token}"))], false),
        [ForeignCredentialKind::NpmToken]
    );
    let broken = with_broken_checksum(&token);
    assert_eq!(
        kinds(&[("authorization", &format!("Bearer {broken}"))], false),
        NONE
    );
}

#[test]
fn a_gitlab_token_is_read_by_prefix_and_shape_including_the_private_token_header() {
    let token = token(TokenKind::Gitlab, "lab");
    let token = token.as_str();
    assert_eq!(
        kinds(&[("authorization", &format!("Bearer {token}"))], false),
        [ForeignCredentialKind::GitlabToken]
    );
    assert_eq!(
        kinds(&[("private-token", token)], false),
        [ForeignCredentialKind::GitlabToken]
    );
    assert_eq!(
        kinds(&[("authorization", "Bearer glpat-short")], false),
        NONE
    );
}

#[test]
fn a_newer_azure_devops_token_is_named_inside_basic_credentials() {
    let mut token = "B".repeat(AZURE_LEN);
    token.replace_range(AZURE_MARK_AT..AZURE_MARK_AT + 4, "AZDO");
    let basic = format!("Basic {}", STANDARD.encode(format!(":{token}")));
    assert_eq!(
        kinds(&[("authorization", &basic)], false),
        [ForeignCredentialKind::AzureDevopsToken]
    );
    let mut unmarked = token.clone();
    unmarked.replace_range(AZURE_MARK_AT..AZURE_MARK_AT + 4, "ABCD");
    let basic = format!("Basic {}", STANDARD.encode(format!(":{unmarked}")));
    assert_eq!(kinds(&[("authorization", &basic)], false), NONE);
}

#[test]
fn a_known_token_as_the_password_of_basic_credentials_is_named() {
    let token = with_checksum("ghp_", ENTROPY);
    let basic = format!(
        "Basic {}",
        STANDARD.encode(format!("x-access-token:{token}"))
    );
    assert_eq!(
        kinds(&[("authorization", &basic)], true),
        [ForeignCredentialKind::GithubToken]
    );
}

#[test]
fn a_token_after_basic_that_is_not_base64_is_read_as_it_stands() {
    let token = with_checksum("ghp_", ENTROPY);
    assert_eq!(
        kinds(&[("authorization", &format!("Basic {token}"))], false),
        [ForeignCredentialKind::GithubToken]
    );
}

#[derive(Default)]
struct Seen(Mutex<Vec<Event>>);

impl EventSink for Seen {
    fn emit(&self, event: Event) {
        self.0.lock().unwrap().push(event);
    }
}

fn note(
    warning: &ForeignCredentialWarning,
    host: &str,
    sink: &Seen,
    flagged: &mut Vec<String>,
    kinds: &[ForeignCredentialKind],
) {
    warning.note(
        &WorkspaceId::new("box").unwrap(),
        &Host::parse_normalised(host).unwrap(),
        sink,
        flagged,
        kinds,
    );
}

#[test]
fn a_finding_is_flagged_once_and_raised_once_for_each_host_and_kind() {
    let warning = ForeignCredentialWarning::default();
    let sink = Seen::default();
    let mut flagged = Vec::new();
    let kinds = [
        ForeignCredentialKind::GithubToken,
        ForeignCredentialKind::NpmToken,
    ];
    note(&warning, "a.test", &sink, &mut flagged, &kinds);
    note(&warning, "a.test", &sink, &mut flagged, &kinds);
    assert_eq!(flagged, ["github_token", "npm_token"]);
    assert_eq!(sink.0.lock().unwrap().len(), 2);
    note(&warning, "b.test", &sink, &mut flagged, &kinds[..1]);
    assert_eq!(flagged.len(), 2);
    assert_eq!(sink.0.lock().unwrap().len(), 3);
}

#[test]
fn nothing_is_noted_when_the_warning_is_off_or_nothing_was_found() {
    let sink = Seen::default();
    let mut flagged = Vec::new();
    let kinds = [ForeignCredentialKind::GithubToken];
    let off = ForeignCredentialWarning::new(|| false);
    assert!(!off.is_on());
    note(&off, "a.test", &sink, &mut flagged, &kinds);
    let on = ForeignCredentialWarning::default();
    note(&on, "a.test", &sink, &mut flagged, &[]);
    assert_eq!(flagged, Vec::<String>::new());
    assert_eq!(sink.0.lock().unwrap().len(), 0);
}

#[test]
fn the_remembered_findings_stop_at_the_limit() {
    let warning = ForeignCredentialWarning::default();
    let sink = Seen::default();
    let mut flagged = Vec::new();
    for i in 0..=MAX_SEEN {
        note(
            &warning,
            &format!("h{i}.test"),
            &sink,
            &mut flagged,
            &[ForeignCredentialKind::GithubToken],
        );
    }
    assert_eq!(sink.0.lock().unwrap().len(), MAX_SEEN);
}

#[test]
fn the_debug_form_names_the_type_and_nothing_else() {
    let text = format!("{:?}", ForeignCredentialWarning::default());
    assert!(text.contains("ForeignCredentialWarning"), "{text}");
}

#[test]
fn a_value_of_exactly_the_limit_is_read_and_one_byte_more_is_not() {
    let token = with_checksum("ghp_", ENTROPY);
    let at_limit = format!("Bearer {token}{}", " ".repeat(MAX_VALUE - token.len() - 7));
    assert_eq!(at_limit.len(), MAX_VALUE);
    let named = [ForeignCredentialKind::GithubToken];
    assert_eq!(kinds(&[("authorization", &at_limit)], false), named);
    let over = format!("{at_limit} ");
    assert_eq!(kinds(&[("authorization", &over)], false), NONE);
}

#[test]
fn a_checksummed_body_with_a_character_outside_base62_is_not_a_token() {
    let token = with_checksum("ghp_", "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o-");
    assert_eq!(
        kinds(&[("authorization", &format!("Bearer {token}"))], false),
        NONE
    );
}

#[test]
fn any_authorization_on_a_git_path_counts_and_off_a_git_path_it_stays_silent() {
    let value = "Bearer an-opaque-value-of-no-known-kind";
    assert_eq!(
        kinds(&[("authorization", value)], true),
        [ForeignCredentialKind::GitAuthorization]
    );
    assert_eq!(kinds(&[("authorization", value)], false), NONE);
    // Another header never counts as an Authorization.
    assert_eq!(kinds(&[("x-custom", value)], true), NONE);
    // Nothing to name when the value is empty.
    assert_eq!(kinds(&[("authorization", "")], true), NONE);
}

#[test]
fn a_stale_stand_in_is_left_to_its_own_check_and_not_counted_as_an_authorization() {
    let stale_one = "puddle-secret-API_KEY-00112233445566778899aabbccddeeff";
    assert_eq!(
        kinds(&[("authorization", &format!("Bearer {stale_one}"))], true),
        NONE
    );
    let wrong_sum = format!("gho_{ENTROPY}AAAAAA");
    assert_eq!(
        kinds(&[("authorization", &format!("token {wrong_sum}"))], true),
        NONE
    );
}

#[test]
fn a_header_that_holds_one_of_the_workspaces_stand_ins_is_not_looked_at() {
    let token = with_checksum("ghp_", ENTROPY);
    let map = headers(&[("authorization", &format!("Bearer {token}"))]);
    let held = kinds_in_headers(&map, &[], true, &|_| true);
    assert_eq!(held, NONE);
}

#[test]
fn a_header_puddle_injected_is_not_looked_at() {
    let token = with_checksum("ghp_", ENTROPY);
    let map = headers(&[("authorization", &format!("Bearer {token}"))]);
    let skip = [&AUTHORIZATION];
    assert_eq!(kinds_in_headers(&map, &skip, true, &|_| false), NONE);
}

#[test]
fn each_kind_is_named_once_and_in_header_order() {
    let github = with_checksum("ghp_", ENTROPY);
    let npm = with_checksum("npm_", ENTROPY);
    let gitlab = token(TokenKind::Gitlab, "lab");
    let map = headers(&[
        ("authorization", &format!("Bearer {github}")),
        ("authorization", &format!("Bearer {github}")),
        ("private-token", &gitlab),
        ("authorization", &format!("Bearer {npm}")),
    ]);
    assert_eq!(
        kinds_in_headers(&map, &[], false, &|_| false),
        [
            ForeignCredentialKind::GithubToken,
            ForeignCredentialKind::NpmToken,
            ForeignCredentialKind::GitlabToken,
        ]
    );
}

#[test]
fn a_value_over_the_limit_is_not_read() {
    let long = format!("Bearer {}", "a".repeat(MAX_VALUE));
    assert_eq!(kinds(&[("authorization", &long)], true), NONE);
}
