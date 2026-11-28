// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a token endpoint's answer becomes: the stand-ins the tool gets, what the profiles read
//! and refuse, and how a request is matched to its endpoint.

use super::*;
use crate::test_rig::*;

#[tokio::test]
async fn a_claude_login_keeps_the_tokens_and_the_tool_gets_stand_ins_with_their_shape() {
    let rig = rig();
    let given = claude_login(&rig, REAL_ACCESS, REAL_REFRESH).await;

    let access = given["access_token"].as_str().unwrap();
    let refresh = given["refresh_token"].as_str().unwrap();
    assert_ne!(access, REAL_ACCESS);
    assert_ne!(refresh, REAL_REFRESH);
    assert!(access.starts_with("sk-ant-oat01-") && access.len() == REAL_ACCESS.len());
    assert!(refresh.starts_with("sk-ant-ort01-") && refresh.len() == REAL_REFRESH.len());
    // Everything else in the answer is the service's.
    assert_eq!(given["expires_in"], 28800);
    assert_eq!(given["token_type"], "Bearer");
    assert_eq!(given["scope"], "user:inference user:profile");
    assert_eq!(given["account"]["email_address"], "me@example.invalid");
    assert!(!given.to_string().contains("CANARY"));

    assert_eq!(rig.registry.len(), 2);
    assert_eq!(rig.registry.real_of(access).as_deref(), Some(REAL_ACCESS));
    assert_eq!(rig.registry.real_of(refresh).as_deref(), Some(REAL_REFRESH));
    assert_eq!(rig.notices.seen(), []);
    let v = vault(&rig);
    let kept = v.read(&CLAUDE, Role::Access).unwrap().unwrap();
    assert_eq!(
        (kept.stand_in.as_str(), kept.real.as_str()),
        (access, REAL_ACCESS)
    );
    let kept = v.read(&CLAUDE, Role::Refresh).unwrap().unwrap();
    assert_eq!(
        (kept.stand_in.as_str(), kept.real.as_str()),
        (refresh, REAL_REFRESH)
    );
    let kept = rig.logins.kept();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].profile, "claude");
    assert_eq!(kept[0].roles, [Role::Access, Role::Refresh]);
}

/// Claude Code 2.1.5 and older exchange the code and refresh on the console host. The answer
/// is the one the newer host gives; the request bodies are what that version sends.
#[tokio::test]
async fn an_old_claude_code_login_on_the_console_host_is_kept_like_a_new_one() {
    let rig = rig();
    let given = claude_login_at(&rig, "console.anthropic.com", REAL_ACCESS, REAL_REFRESH).await;
    let access = given["access_token"].as_str().unwrap();
    let refresh = given["refresh_token"].as_str().unwrap();
    assert!(access.starts_with("sk-ant-oat01-") && access.len() == REAL_ACCESS.len());
    assert!(refresh.starts_with("sk-ant-ort01-") && refresh.len() == REAL_REFRESH.len());
    assert!(!given.to_string().contains("CANARY"));
    assert_eq!(given["expires_in"], 28800);
    assert_eq!(rig.registry.len(), 2);
    assert_eq!(rig.notices.seen(), []);
    let kept = vault(&rig).read(&CLAUDE, Role::Access).unwrap().unwrap();
    assert_eq!(kept.real.as_str(), REAL_ACCESS);

    // The refresh the old version sends carries the stand-in, swapped back toward the console host.
    let exchange = rig
        .logins
        .begin(&host("console.anthropic.com"), "POST", "/v1/oauth/token")
        .unwrap();
    assert_eq!(exchange.swap_fields(), ["refresh_token"]);

    // A login on either host fills the same slots: one stand-in each.
    let again = claude_login_at(&rig, "platform.claude.com", ACCESS_B, REFRESH_B).await;
    assert_eq!(again["access_token"], given["access_token"]);
    assert_eq!(rig.registry.len(), 2);
}

#[tokio::test]
async fn a_refresh_gives_the_tool_the_same_stand_ins_with_new_tokens_behind_them() {
    let rig = rig();
    let first = claude_login(&rig, REAL_ACCESS, REAL_REFRESH).await;
    let second_access =
        "sk-ant-oat01-CANARY0access0second0EEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE";
    let second_refresh = "sk-ant-ort01-CANARY0refresh0second0FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF";
    let again = claude_login(&rig, second_access, second_refresh).await;
    assert_eq!(first["access_token"], again["access_token"]);
    assert_eq!(first["refresh_token"], again["refresh_token"]);
    // The registry, which the proxy swaps from, holds the new tokens, not the first ones.
    let access = again["access_token"].as_str().unwrap();
    let refresh = again["refresh_token"].as_str().unwrap();
    assert_eq!(rig.registry.real_of(access).as_deref(), Some(second_access));
    assert_eq!(
        rig.registry.real_of(refresh).as_deref(),
        Some(second_refresh)
    );
    assert_eq!(rig.registry.len(), 2, "a slot has one stand-in");
    let kept = vault(&rig).read(&CLAUDE, Role::Access).unwrap().unwrap();
    assert!(kept.real.contains("second"));
    assert_eq!(kept.stand_in, again["access_token"].as_str().unwrap());
}

#[tokio::test]
async fn a_token_endpoint_host_is_known_by_name_and_only_while_capture_is_on() {
    let on = rig();
    for name in ["platform.claude.com", "console.anthropic.com", "github.com"] {
        assert!(on.logins.is_token_endpoint(&host(name)), "{name}");
    }
    for name in ["example.com", "evil.platform.claude.com"] {
        assert!(!on.logins.is_token_endpoint(&host(name)), "{name}");
    }
    let off = rig_on(&Arc::new(MemoryStore::new()), false);
    assert!(!off.logins.is_token_endpoint(&host("platform.claude.com")));
}

#[tokio::test]
async fn the_token_endpoint_asks_for_the_refresh_swap_and_other_requests_are_not_exchanges() {
    let rig = rig();
    let mut exchange = rig
        .logins
        .begin(&host("platform.claude.com"), "POST", "/v1/oauth/token")
        .unwrap();
    assert_eq!(exchange.swap_fields(), ["refresh_token"]);
    assert!(exchange.answer_mut().is_some());
    let github = rig
        .logins
        .begin(&host("github.com"), "POST", "/login/oauth/access_token")
        .unwrap();
    assert_eq!(github.swap_fields(), ["refresh_token"]);
    // The key endpoint answers with a key and takes no refresh token.
    let key = rig
        .logins
        .begin(
            &host("api.anthropic.com"),
            "POST",
            "/api/oauth/claude_cli/create_api_key",
        )
        .unwrap();
    assert_eq!(key.swap_fields().len(), 0);

    for (name, method, path) in [
        ("platform.claude.com", "GET", "/v1/oauth/token"),
        ("console.anthropic.com", "GET", "/v1/oauth/token"),
        ("console.anthropic.com", "POST", "/v1/oauth/authorize"),
        ("platform.claude.com", "POST", "/v1/oauth/authorize"),
        ("platform.claude.com", "POST", "/v1/oauth/token/extra"),
        ("api.anthropic.com", "POST", "/v1/oauth/token"),
        ("github.com", "POST", "/org/repo.git/git-receive-pack"),
        ("github.com", "POST", "/login/device/code"),
        ("example.com", "POST", "/v1/oauth/token"),
    ] {
        assert!(
            rig.logins.begin(&host(name), method, path).is_none(),
            "{method} {name}{path}"
        );
    }
}

#[tokio::test]
async fn hostile_hg33_another_spelling_of_the_token_endpoint_is_still_read() {
    let rig = rig();
    for path in [
        "/v1/oauth/token/",
        "//v1//oauth/token",
        "/v1/./oauth/token",
        "/v1/x/../oauth/token",
        "/V1/OAuth/Token",
        "/v1/oauth/%74oken",
        "/v1/oauth/token;x=1",
        "/v1/oauth/token?grant=refresh_token",
        "\\v1\\oauth\\token",
    ] {
        assert!(
            rig.logins
                .begin(&host("platform.claude.com"), "POST", path)
                .is_some(),
            "{path}"
        );
    }
}

#[tokio::test]
async fn a_github_form_answer_stays_a_form_and_the_scope_and_type_are_kept() {
    let rig = rig();
    let exchange = rig
        .logins
        .begin(&host("github.com"), "POST", "/login/oauth/access_token");
    let body = answered_bytes(
        exchange,
        200,
        Some("application/x-www-form-urlencoded; charset=utf-8"),
        format!("access_token={REAL_GH}&scope=repo%2Cread%3Aorg%2Cgist&token_type=bearer")
            .as_bytes(),
    )
    .await
    .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    let (first, rest) = text.split_once('&').unwrap();
    assert_eq!(rest, "scope=repo%2Cread%3Aorg%2Cgist&token_type=bearer");
    let stand_in = first.strip_prefix("access_token=").unwrap();
    assert!(stand_in.starts_with("gho_") && stand_in.len() == REAL_GH.len());
    assert_ne!(stand_in, REAL_GH);
    assert_eq!(rig.registry.len(), 1);
    assert_eq!(
        vault(&rig)
            .read(&GITHUB, Role::Access)
            .unwrap()
            .unwrap()
            .real
            .as_str(),
        REAL_GH
    );
}

#[tokio::test]
async fn a_github_json_answer_stays_json() {
    let rig = rig();
    let exchange = rig
        .logins
        .begin(&host("github.com"), "POST", "/login/oauth/access_token");
    let body = answered_bytes(
        exchange,
        200,
        Some("application/json; charset=utf-8"),
        json!({"access_token": REAL_GH, "token_type": "bearer", "scope": "gist,repo"})
            .to_string()
            .as_bytes(),
    )
    .await
    .unwrap();
    let given: Value = serde_json::from_slice(&body).unwrap();
    assert!(given["access_token"].as_str().unwrap().starts_with("gho_"));
    assert_eq!(given["scope"], "gist,repo");
}

#[tokio::test]
async fn the_api_key_a_login_creates_keeps_its_last_twenty_characters() {
    let rig = rig();
    let exchange = rig.logins.begin(
        &host("api.anthropic.com"),
        "POST",
        "/api/oauth/claude_cli/create_api_key",
    );
    let body = answered_bytes(
        exchange,
        200,
        Some("application/json"),
        json!({"raw_key": REAL_KEY}).to_string().as_bytes(),
    )
    .await
    .unwrap();
    let given: Value = serde_json::from_slice(&body).unwrap();
    let key = given["raw_key"].as_str().unwrap();
    assert!(key.starts_with("sk-ant-api03-"));
    assert_eq!(key.len(), REAL_KEY.len());
    assert!(key.ends_with(&REAL_KEY[REAL_KEY.len() - 20..]));
    assert_ne!(key, REAL_KEY);
    assert_eq!(rig.logins.kept()[0].roles, [Role::ApiKey]);
}

#[tokio::test]
async fn answers_without_a_token_are_left_alone_and_say_nothing() {
    let rig = rig();
    for (status, content_type, body) in [
        // GitHub's device flow answers 200 with an error until the user approves.
        (
            200,
            "application/x-www-form-urlencoded",
            "error=authorization_pending&interval=5",
        ),
        (
            200,
            "application/json",
            r#"{"error":"authorization_pending"}"#,
        ),
        (200, "application/json", "{}"),
        (200, "application/json", r#"{"access_token":""}"#),
        (200, "application/json", r#"{"access_token":12345}"#),
        // Errors are the tool's to read; nothing is looked at.
        (400, "application/json", r#"{"error":"invalid_grant"}"#),
        (401, "text/html", "no"),
        (204, "application/json", ""),
    ] {
        let exchange = rig
            .logins
            .begin(&host("github.com"), "POST", "/login/oauth/access_token");
        assert_eq!(
            rewritten(exchange, status, Some(content_type), body.as_bytes()).await,
            Rewrite::Keep,
            "{status} {body}"
        );
    }
    assert_eq!(rig.registry.len(), 0);
    assert_eq!(rig.notices.seen(), []);
}

#[tokio::test]
async fn a_success_answer_puddle_cannot_read_is_passed_on_and_the_user_is_told() {
    let rig = rig();
    for (content_type, encoding, length) in [
        (Some("text/html"), None, Some(10)),
        (None, None, Some(10)),
        (Some("application/json"), Some("gzip"), Some(10)),
        (Some("application/json"), None, Some(10 * 1024 * 1024)),
    ] {
        let mut exchange = rig
            .logins
            .begin(&host("platform.claude.com"), "POST", "/v1/oauth/token")
            .unwrap();
        let answer = exchange.answer_mut().unwrap();
        let head = AnswerHead {
            status: 200,
            content_type,
            content_encoding: encoding,
            content_length: length,
        };
        assert!(
            matches!(answer.wants(&head), Want::Refuse(r) if r.code() == ReasonCode::LoginNotKept),
            "{content_type:?} {encoding:?} {length:?}"
        );
    }
    assert_eq!(rig.notices.seen().len(), 4);
    assert!(
        rig.notices
            .seen()
            .iter()
            .all(|(profile, problem)| profile == "claude" && *problem == Problem::UnexpectedAnswer)
    );
}

#[tokio::test]
async fn an_answer_that_does_not_parse_is_passed_on_and_the_user_is_told() {
    let rig = rig();
    let exchange = rig
        .logins
        .begin(&host("platform.claude.com"), "POST", "/v1/oauth/token");
    refused(
        exchange,
        200,
        Some("application/json"),
        b"{\"access_token\": ",
    )
    .await;
    assert_eq!(
        rig.notices.seen(),
        [("claude".to_owned(), Problem::UnexpectedAnswer)]
    );
    assert_eq!(rig.registry.len(), 0);
    assert!(vault(&rig).read(&CLAUDE, Role::Access).unwrap().is_none());
}

#[tokio::test]
async fn a_token_bound_to_a_key_cannot_be_captured() {
    let rig = rig();
    let exchange = rig
        .logins
        .begin(&host("platform.claude.com"), "POST", "/v1/oauth/token");
    let body = json!({"access_token": REAL_ACCESS, "token_type": "DPoP"}).to_string();
    refused(exchange, 200, Some("application/json"), body.as_bytes()).await;
    assert_eq!(
        rig.notices.seen(),
        [("claude".to_owned(), Problem::BoundToken)]
    );
    assert_eq!(rig.registry.len(), 0);
}

#[tokio::test]
async fn a_token_a_stand_in_cannot_copy_is_passed_on_and_the_user_is_told() {
    let rig = rig();
    for token in ["short", "has a space in it, 0123456789"] {
        let exchange = rig
            .logins
            .begin(&host("platform.claude.com"), "POST", "/v1/oauth/token");
        let body = json!({"access_token": token}).to_string();
        refused(exchange, 200, Some("application/json"), body.as_bytes()).await;
    }
    assert_eq!(
        rig.notices.seen(),
        [
            ("claude".to_owned(), Problem::UnusableToken),
            ("claude".to_owned(), Problem::UnusableToken)
        ]
    );
    assert_eq!(rig.registry.len(), 0);
}

#[tokio::test]
async fn when_the_credential_store_is_not_there_the_login_passes_and_nothing_is_half_done() {
    let rig = rig();
    rig.store.break_it();
    let exchange = rig
        .logins
        .begin(&host("platform.claude.com"), "POST", "/v1/oauth/token");
    refused(
        exchange,
        200,
        Some("application/json"),
        &claude_answer(REAL_ACCESS, REAL_REFRESH),
    )
    .await;
    assert_eq!(
        rig.notices.seen(),
        [("claude".to_owned(), Problem::StoreUnavailable)]
    );
    assert_eq!(
        rig.registry.len(),
        0,
        "no stand-in for a token that was not kept"
    );
    assert_eq!(rig.logins.kept().len(), 0);
}

#[test]
fn a_request_is_matched_to_its_token_endpoint_in_any_spelling() {
    use crate::token_endpoint_of;
    let (profile, endpoint) =
        token_endpoint_of("Platform.Claude.com", "POST", "//V1/oauth/./token?x=1").unwrap();
    assert_eq!(
        (profile.id, endpoint.host),
        ("claude", "platform.claude.com")
    );
    let (profile, _) =
        token_endpoint_of("login.microsoftonline.com", "POST", "/t1/oauth2/v2.0/token").unwrap();
    assert_eq!(profile.id, "entra");
    // Another method, another host on the same path, another path: none of them.
    assert!(token_endpoint_of("platform.claude.com", "GET", "/v1/oauth/token").is_none());
    assert!(token_endpoint_of("api.anthropic.com", "POST", "/v1/oauth/token").is_none());
    assert!(token_endpoint_of("platform.claude.com", "POST", "/v1/oauth").is_none());
    // A server that drops `;parameters` from each segment reads this as the token endpoint.
    let (profile, _) =
        token_endpoint_of("platform.claude.com", "POST", "/v1;x=1/oauth/token").unwrap();
    assert_eq!(profile.id, "claude");
}

#[test]
fn canonical_paths_collapse_every_spelling_to_one() {
    use crate::logins::canonical_path;
    assert_eq!(canonical_path("/a/b"), "/a/b");
    assert_eq!(canonical_path(""), "/");
    assert_eq!(canonical_path("/"), "/");
    assert_eq!(canonical_path("/A//b/./c/../d/"), "/a/b/d");
    assert_eq!(canonical_path("/%41%62"), "/ab");
    assert_eq!(canonical_path("/a%2fb"), "/a/b");
    assert_eq!(canonical_path("/a%zz"), "/a%zz");
    assert_eq!(canonical_path("/a%"), "/a%");
    assert_eq!(canonical_path("/a;x/b?q#f"), "/a/b");
    assert_eq!(canonical_path("/a;x=1;y/b;z/c;"), "/a/b/c");
    assert_eq!(canonical_path("/a/b/..;x/c"), "/a/c");
    assert_eq!(canonical_path("/../../a"), "/a");
    assert_eq!(canonical_path("/%ff"), "/\u{fffd}");
}

#[tokio::test]
async fn loading_twice_registers_each_stand_in_once() {
    let first = rig();
    claude_login(&first, REAL_ACCESS, REAL_REFRESH).await;
    let second = rig_on(&first.store, true);
    second.logins.load().await;
    second.logins.load().await;
    assert_eq!(second.registry.len(), 2);
    assert_eq!(second.notices.seen(), []);
}

#[tokio::test]
async fn a_saved_pair_the_registry_refuses_is_reported_as_unreadable() {
    let first = rig();
    // A stand-in too short to be one: only a damaged or foreign entry looks like this.
    vault(&first)
        .write(&CLAUDE, Role::Access, "short", REAL_ACCESS)
        .unwrap();
    first.logins.load().await;
    assert_eq!(first.registry.len(), 0);
    assert_eq!(
        first.notices.seen(),
        [("claude".to_owned(), Problem::Unreadable)]
    );
}

#[tokio::test]
async fn a_refresh_with_a_token_a_header_cannot_carry_is_passed_on_and_the_user_is_told() {
    let rig = rig();
    claude_login(&rig, REAL_ACCESS, REAL_REFRESH).await;
    let exchange = rig
        .logins
        .begin(&host("platform.claude.com"), "POST", "/v1/oauth/token");
    let body = json!({"access_token": "has a space in it, 0123456789"}).to_string();
    refused(exchange, 200, Some("application/json"), body.as_bytes()).await;
    assert_eq!(
        rig.notices.seen(),
        [("claude".to_owned(), Problem::UnusableToken)]
    );
    // What was kept is still what the workspace holds.
    assert_eq!(rig.registry.len(), 2);
    let kept = vault(&rig).read(&CLAUDE, Role::Access).unwrap().unwrap();
    assert_eq!(kept.real.as_str(), REAL_ACCESS);
}

#[tokio::test]
async fn a_slot_the_registry_lost_is_made_again_by_the_next_answer() {
    let rig = rig();
    let first = claude_login(&rig, REAL_ACCESS, REAL_REFRESH).await;
    let stand_in = first["access_token"].as_str().unwrap();
    assert!(rig.registry.remove(stand_in), "something else removed it");
    assert_eq!(rig.registry.len(), 1);
    let again = claude_login(&rig, ACCESS_B, REFRESH_B).await;
    assert_eq!(again["access_token"], stand_in, "the slot's stand-in stays");
    assert_eq!(rig.registry.len(), 2);
}

mod properties {
    use std::fmt::Write as _;

    use proptest::prelude::*;

    use crate::logins::canonical_path;

    proptest! {
        #[test]
        fn no_path_a_guest_sends_panics_and_the_spelling_is_always_plain(path in "\\PC{0,80}") {
            let spelled = canonical_path(&path);
            prop_assert!(spelled.starts_with('/'));
            prop_assert!(!spelled.contains("//"));
            prop_assert!(!spelled.contains("/./"));
            prop_assert!(!spelled.contains("/../"));
            prop_assert!(!spelled.contains('\\'));
            prop_assert_eq!(spelled.clone(), spelled.to_ascii_lowercase());
        }

        #[test]
        fn the_token_endpoint_is_found_in_every_spelling_of_its_letters(
            upper in prop::collection::vec(any::<bool>(), 14),
            encode in prop::collection::vec(any::<bool>(), 14),
            slashes in 1_usize..4,
        ) {
            // "/v1/oauth/token" with each letter in either case and either written as itself or
            // percent-encoded, and the slashes doubled.
            let mut spelled = String::new();
            for (i, c) in "v1/oauth/token".chars().enumerate() {
                if c == '/' {
                    spelled.push_str(&"/".repeat(slashes));
                    continue;
                }
                let c = if upper.get(i).copied().unwrap_or(false) { c.to_ascii_uppercase() } else { c };
                if encode.get(i).copied().unwrap_or(false) {
                    let _ = write!(spelled, "%{:02x}", u32::from(c));
                } else {
                    spelled.push(c);
                }
            }
            let path = format!("/{spelled}");
            prop_assert_eq!(canonical_path(&path), "/v1/oauth/token");
        }
    }
}

#[tokio::test]
async fn a_long_text_in_a_field_named_like_an_address_or_a_kind_is_not_a_token() {
    let rig = rig();
    let mut answer: Value =
        serde_json::from_slice(&claude_answer(REAL_ACCESS, REAL_REFRESH)).unwrap();
    answer["token_endpoint"] = json!("https://example.invalid/a/long/address/for/the/token");
    let exchange = rig
        .logins
        .begin(&host("platform.claude.com"), "POST", "/v1/oauth/token");
    let body = answered_bytes(
        exchange,
        200,
        Some("application/json"),
        answer.to_string().as_bytes(),
    )
    .await;
    assert!(body.is_some());
    assert_eq!(rig.notices.seen(), []);
}

#[tokio::test]
async fn a_long_token_in_a_field_the_profile_does_not_name_is_refused() {
    let rig = rig();
    let mut answer: Value =
        serde_json::from_slice(&claude_answer(REAL_ACCESS, REAL_REFRESH)).unwrap();
    answer["id_token"] = json!("abcdefghijklmnopqrstuvwxyz0123456789");
    let exchange = rig
        .logins
        .begin(&host("platform.claude.com"), "POST", "/v1/oauth/token");
    refused(
        exchange,
        200,
        Some("application/json"),
        answer.to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        rig.notices.seen(),
        [("claude".to_owned(), Problem::UnnamedToken)]
    );
}

#[tokio::test]
async fn an_identifier_in_a_token_named_field_does_not_stop_the_login() {
    let rig = rig();
    let mut answer: Value =
        serde_json::from_slice(&claude_answer(REAL_ACCESS, REAL_REFRESH)).unwrap();
    answer["token_uuid"] = json!("7a1b2c3d-0e4f-4a5b-8c6d-9e0f1a2b3c4d");
    let exchange = rig
        .logins
        .begin(&host("platform.claude.com"), "POST", "/v1/oauth/token");
    let body = answered_bytes(
        exchange,
        200,
        Some("application/json"),
        answer.to_string().as_bytes(),
    )
    .await
    .expect("the login passes");
    let given: Value = serde_json::from_slice(&body).unwrap();
    assert_ne!(given["access_token"], REAL_ACCESS);
    assert_eq!(given["token_uuid"], "7a1b2c3d-0e4f-4a5b-8c6d-9e0f1a2b3c4d");
    assert_eq!(rig.notices.seen(), []);
}
