// SPDX-License-Identifier: AGPL-3.0-or-later
//! The readers of `puddle-repos` against GitHub's own description of the REST API.
//!
//! `tests/data/github-rest-slice.json` is the part of github/rest-api-description (the
//! `2022-11-28` version Puddle asks for, pinned by commit in `ci/github-rest/pin.txt`) that the
//! readers use: `GET /user`, `GET /user/repos` (what a classic, a fine-grained and an app token
//! list; the same operation) and `GET /user/orgs`. The tests do three things with it:
//!
//! 1. answer the readers with what the description says GitHub answers (every field, then only the
//!    required ones) and check what they make of it;
//! 2. check that what the readers ask (query names, values, headers they read) is something the
//!    description allows;
//! 3. check the hand-written fixtures of the other tests against the description's types, so a
//!    fixture cannot drift into a shape GitHub never sends.

#![expect(
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a panic is how a test fails"
)]

use std::collections::BTreeSet;

use puddle_repos::{ApiReply, Freshness, ListState, Role, Visibility};
use serde_json::{Map, Value, json};

use crate::common::{REPOS_PATH, Rig, binding, data, gh, identities, ok_json};

const ORGS: &str = "/user/orgs?per_page=100";

fn slice() -> Value {
    serde_json::from_slice(&data("github-rest-slice.json")).unwrap()
}

fn follow<'a>(root: &'a Value, schema: &'a Value) -> &'a Value {
    match schema.get("$ref").and_then(Value::as_str) {
        Some(reference) => {
            let mut node = root;
            for part in reference.trim_start_matches("#/").split('/') {
                node = node
                    .get(part)
                    .unwrap_or_else(|| panic!("{reference} is not in the slice"));
            }
            follow(root, node)
        }
        None => schema,
    }
}

/// The schema of the body GitHub answers `path` with (`200`).
fn body_schema<'a>(root: &'a Value, path: &str) -> &'a Value {
    &root["paths"][path]["get"]["responses"]["200"]["content"]["application/json"]["schema"]
}

/// What `schema` allows, checked loosely: types, required fields (only when `strict`), `enum`,
/// `nullable`, `allOf`, `oneOf` and `anyOf` (at least one branch). Formats and extra fields are
/// not checked.
fn check(
    root: &Value,
    schema: &Value,
    value: &Value,
    strict: bool,
    at: &str,
    errors: &mut Vec<String>,
) {
    let schema = follow(root, schema);
    if value.is_null() && schema["nullable"] == json!(true) {
        return;
    }
    for branch in schema["allOf"].as_array().into_iter().flatten() {
        check(root, branch, value, strict, at, errors);
    }
    for key in ["oneOf", "anyOf"] {
        let Some(branches) = schema[key].as_array() else {
            continue;
        };
        let mut best: Option<Vec<String>> = None;
        for branch in branches {
            let mut found = Vec::new();
            check(root, branch, value, strict, at, &mut found);
            if found.is_empty() {
                best = None;
                break;
            }
            if best.as_ref().is_none_or(|b| found.len() < b.len()) {
                best = Some(found);
            }
        }
        errors.extend(best.unwrap_or_default());
    }
    let kind = schema["type"].as_str();
    let ok = match kind {
        Some("string") => value.is_string(),
        Some("integer") => value.is_i64() || value.is_u64(),
        Some("number") => value.is_number(),
        Some("boolean") => value.is_boolean(),
        Some("array") => value.is_array(),
        Some("object") => value.is_object(),
        _ => true,
    };
    if !ok {
        errors.push(format!("{at}: {value} is not a {}", kind.unwrap_or("?")));
        return;
    }
    if let Some(allowed) = schema["enum"]
        .as_array()
        .filter(|a| !value.is_null() && !a.contains(value))
    {
        errors.push(format!("{at}: {value} is not one of {allowed:?}"));
    }
    if let (Some(item), Some(items)) = (schema.get("items"), value.as_array()) {
        for (i, entry) in items.iter().enumerate() {
            check(root, item, entry, strict, &format!("{at}[{i}]"), errors);
        }
    }
    if let Some(object) = value.as_object() {
        if strict {
            for name in schema["required"].as_array().into_iter().flatten() {
                let name = name.as_str().unwrap();
                if !object.contains_key(name) {
                    errors.push(format!("{at}: required `{name}` is missing"));
                }
            }
        }
        for (name, entry) in object {
            if let Some(property) = schema["properties"].get(name) {
                check(
                    root,
                    property,
                    entry,
                    strict,
                    &format!("{at}.{name}"),
                    errors,
                );
            }
        }
    }
}

/// An answer the description allows: `full` has every property, otherwise only the required ones.
/// A value comes from the description's own `example` when it has one.
fn sample(root: &Value, schema: &Value, full: bool) -> Value {
    let schema = follow(root, schema);
    if let Some(branch) = schema["allOf"].as_array() {
        let mut merged = Map::new();
        for part in branch {
            if let Value::Object(object) = sample(root, part, full) {
                merged.extend(object);
            }
        }
        return Value::Object(merged);
    }
    if let Some(first) = schema["oneOf"]
        .as_array()
        .or_else(|| schema["anyOf"].as_array())
        .and_then(|b| b.first())
    {
        return sample(root, first, full);
    }
    if let Some(example) = schema
        .get("example")
        .filter(|e| !e.is_object() && !e.is_array())
    {
        return example.clone();
    }
    match schema["type"].as_str() {
        Some("object") => {
            let required: BTreeSet<&str> = schema["required"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let mut out = Map::new();
            for (name, property) in schema["properties"].as_object().into_iter().flatten() {
                if full || required.contains(name.as_str()) {
                    out.insert(name.clone(), sample(root, property, full));
                }
            }
            Value::Object(out)
        }
        Some("array") => json!([sample(root, &schema["items"], full)]),
        Some("integer") => json!(7),
        Some("number") => json!(1.5),
        Some("boolean") => schema.get("default").cloned().unwrap_or(json!(false)),
        _ => schema["enum"][0].clone().as_str().map_or_else(
            || match schema["format"].as_str() {
                Some("date-time") => json!("2025-01-02T15:04:05Z"),
                Some("uri") => json!("https://example.org/x"),
                _ => schema.get("default").cloned().unwrap_or_else(|| json!("x")),
            },
            |first| json!(first),
        ),
    }
}

fn page_of(rows: &Value) -> ApiReply {
    ApiReply::new(200, serde_json::to_vec(rows).unwrap())
}

fn repo_with(full_name: &str, visibility: &str, private: bool, perms: [bool; 5]) -> Value {
    let root = slice();
    let mut repo = sample(&root, &root["components"]["schemas"]["repository"], true);
    repo["full_name"] = json!(full_name);
    repo["name"] = json!(full_name.split('/').nth(1).unwrap());
    repo["visibility"] = json!(visibility);
    repo["private"] = json!(private);
    let [admin, maintain, push, triage, pull] = perms;
    repo["permissions"] =
        json!({"admin": admin, "maintain": maintain, "push": push, "triage": triage, "pull": pull});
    repo
}

async fn read_repos(rows: &Value) -> (Rig, Vec<puddle_repos::Repository>) {
    let rig = Rig::new();
    let source = gh("github.com", "octocat");
    rig.tokens.set(&source, "made-up-token");
    rig.api.reply("api.github.com", REPOS_PATH, page_of(rows));
    let ids = identities(vec![("Me", vec![binding("github.com", source, &[], true)])]);
    let lists = rig.lists(&ids, Freshness::Cached).await;
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0].state, ListState::Ok, "{:?}", lists[0].problem);
    let repos = lists[0].repos.to_vec();
    (rig, repos)
}

#[tokio::test]
async fn the_description_s_own_repository_is_read_in_full_and_with_only_its_required_fields() {
    let root = slice();
    let schema = &root["components"]["schemas"]["repository"];
    for full in [true, false] {
        let repo = sample(&root, schema, full);
        let mut errors = Vec::new();
        check(&root, schema, &repo, true, "repository", &mut errors);
        assert!(
            errors.is_empty(),
            "the sample is not what the description allows: {errors:?}"
        );
        let (_rig, repos) = read_repos(&json!([repo.clone()])).await;
        assert_eq!(repos.len(), 1);
        let read = &repos[0];
        assert_eq!(read.full_name, repo["full_name"].as_str().unwrap());
        assert_eq!(read.archived, repo["archived"] == json!(true));
        assert_eq!(read.fork, repo["fork"] == json!(true));
        assert_eq!(read.url, format!("https://github.com/{}", read.full_name));
        if !full {
            // The required fields name no visibility and no permissions: the reader says what
            // `private` says, and not more than it knows.
            assert_eq!(read.role, Role::Unknown);
            assert_eq!(
                read.visibility,
                if repo["private"] == json!(true) {
                    Visibility::Private
                } else {
                    Visibility::Public
                }
            );
        }
    }
}

#[tokio::test]
async fn every_visibility_and_permission_github_describes_is_mapped_to_one_of_ours() {
    let root = slice();
    let repository = &root["components"]["schemas"]["repository"]["properties"];
    // GitHub describes `visibility` in words ("public, private, or internal"), so the words are
    // what a new value would change.
    let said = repository["visibility"]["description"].as_str().unwrap();
    let permissions = repository["permissions"]["properties"].as_object().unwrap();
    assert_eq!(
        permissions
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["admin", "maintain", "push", "triage", "pull"]),
        "GitHub describes another set of permissions than the five that give a role"
    );
    let rows = [
        (
            "public",
            false,
            [false, false, false, false, true],
            Visibility::Public,
            Role::Read,
        ),
        (
            "private",
            true,
            [false, false, false, true, true],
            Visibility::Private,
            Role::Triage,
        ),
        (
            "internal",
            true,
            [false, false, true, true, true],
            Visibility::Internal,
            Role::Write,
        ),
        (
            "internal",
            true,
            [false, true, true, true, true],
            Visibility::Internal,
            Role::Maintain,
        ),
        (
            "public",
            false,
            [true, true, true, true, true],
            Visibility::Public,
            Role::Admin,
        ),
    ];
    for (visibility, private, perms, want_visibility, want_role) in rows {
        assert!(
            said.contains(visibility),
            "the description no longer says {visibility}: {said}"
        );
        let repo = repo_with("acme/widgets", visibility, private, perms);
        let (_rig, repos) = read_repos(&json!([repo])).await;
        assert_eq!(
            (repos[0].visibility, repos[0].role),
            (want_visibility, want_role),
            "{visibility} {perms:?}"
        );
    }
}

#[tokio::test]
async fn the_account_and_its_organisations_are_read_from_what_the_description_says() {
    let root = slice();
    let user_schema = body_schema(&root, "/user");
    let orgs_schema = body_schema(&root, "/user/orgs");
    for full in [true, false] {
        let mut user = sample(&root, user_schema, full);
        let orgs = sample(&root, orgs_schema, full);
        let mut errors = Vec::new();
        check(&root, user_schema, &user, true, "user", &mut errors);
        check(&root, orgs_schema, &orgs, true, "orgs", &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        // The description's example login is "monalisa octocat" for a name and "octocat" for a login.
        user["login"] = json!("octocat");
        let rig = Rig::new();
        rig.tokens
            .set(&gh("github.com", "octocat"), "made-up-token");
        rig.api.reply("api.github.com", "/user", page_of(&user));
        rig.api.reply("api.github.com", ORGS, page_of(&orgs));
        let read = rig.repos.profile(&gh("github.com", "octocat")).await;
        assert!(read.problem.is_none(), "{:?}", read.problem);
        assert_eq!(read.profile.account.as_deref(), Some("octocat"));
        let id = user["id"].as_u64().unwrap();
        assert_eq!(
            read.profile.email,
            Some(format!("{id}+octocat@users.noreply.github.com"))
        );
        let login = orgs[0]["login"].as_str().unwrap();
        assert_eq!(read.profile.organisations, [login]);
    }
}

#[tokio::test]
async fn what_the_readers_ask_is_what_the_description_allows() {
    let root = slice();
    let operation = &root["paths"]["/user/repos"]["get"];
    let parameters: Vec<&Value> = operation["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| follow(&root, p))
        .filter(|p| p["in"] == "query")
        .collect();
    let rig = Rig::new();
    rig.tokens
        .set(&gh("github.com", "octocat"), "made-up-token");
    rig.api
        .reply("api.github.com", REPOS_PATH, page_of(&json!([])));
    let ids = identities(vec![(
        "Me",
        vec![binding(
            "github.com",
            gh("github.com", "octocat"),
            &[],
            true,
        )],
    )]);
    rig.lists(&ids, Freshness::Cached).await;
    let asked = rig.api.seen();
    assert_eq!(asked.len(), 1);
    let (path, query) = asked[0].path.split_once('?').unwrap();
    assert_eq!(path, "/user/repos");
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=').unwrap();
        let parameter = parameters
            .iter()
            .find(|p| p["name"] == name)
            .unwrap_or_else(|| panic!("`{name}` is not a query parameter of GET /user/repos"));
        let value = if parameter["schema"]["type"] == "integer" {
            json!(value.parse::<u64>().unwrap())
        } else {
            json!(value)
        };
        let mut errors = Vec::new();
        check(&root, &parameter["schema"], &value, true, name, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
    }
    // The reader follows the `Link` header, which the description lists on the 200 answer.
    let headers = operation["responses"]["200"]["headers"]
        .as_object()
        .unwrap();
    assert!(headers.keys().any(|h| h.eq_ignore_ascii_case("link")));
}

#[test]
fn the_hand_written_fixtures_have_the_types_the_description_gives() {
    let root = slice();
    let repository = &root["components"]["schemas"]["repository"];
    let cases = [
        ("github_user_repos_page1.json", repository.clone(), true),
        ("github_user_repos_page2.json", repository.clone(), true),
        (
            "github_user.json",
            body_schema(&root, "/user").clone(),
            false,
        ),
        (
            "github_user_orgs_page1.json",
            root["components"]["schemas"]["organization-simple"].clone(),
            true,
        ),
        (
            "github_user_orgs_page2.json",
            root["components"]["schemas"]["organization-simple"].clone(),
            true,
        ),
    ];
    for (file, schema, is_list) in cases {
        let body: Value = serde_json::from_slice(&data(file)).unwrap();
        let rows = if is_list {
            body.as_array().unwrap().clone()
        } else {
            vec![body]
        };
        for (i, row) in rows.iter().enumerate() {
            let mut errors = Vec::new();
            check(
                &root,
                &schema,
                row,
                false,
                &format!("{file}[{i}]"),
                &mut errors,
            );
            assert!(errors.is_empty(), "{errors:?}");
        }
    }
    // `ok_json` is what the other tests answer with.
    assert_eq!(ok_json("github_user.json").status, 200);
}
