// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_types::Host;

use super::*;

fn host(name: &str) -> Host {
    Host::parse_normalised(name).unwrap()
}

#[test]
fn every_built_in_host_is_a_name_the_proxy_accepts() {
    for profile in builtin() {
        for scope in [Scope::Api, Scope::TokenEndpoint] {
            let set = profile.hosts_for(scope);
            let wanted: Vec<&str> = profile
                .endpoints
                .iter()
                .filter(|e| scope == Scope::Api || !e.request_fields.is_empty())
                .map(|e| e.host)
                .chain(if scope == Scope::Api {
                    profile.api_hosts.iter().copied()
                } else {
                    [].iter().copied()
                })
                .collect();
            for name in wanted {
                let probe = name
                    .strip_prefix("*.")
                    .map_or_else(|| name.to_owned(), |base| format!("anything.{base}"));
                assert!(set.contains(&host(&probe)), "{}: {name}", profile.id);
            }
        }
    }
}

#[test]
fn a_refresh_token_is_swapped_toward_the_token_endpoint_only() {
    let claude = CLAUDE.hosts_for(Scope::TokenEndpoint);
    assert!(claude.contains(&host("platform.claude.com")));
    assert!(claude.contains(&host("console.anthropic.com")));
    assert!(
        !claude.contains(&host("api.anthropic.com")),
        "the endpoint that creates a key takes no refresh token"
    );
    assert!(!claude.contains(&host("claude.ai")));
    let github = GITHUB.hosts_for(Scope::TokenEndpoint);
    assert!(github.contains(&host("github.com")));
    assert!(!github.contains(&host("api.github.com")));
}

#[test]
fn the_github_profile_covers_copilot_hosts_and_not_the_whole_domain() {
    let set = GITHUB.hosts();
    assert!(set.contains(&host("api.individual.githubcopilot.com")));
    assert!(set.contains(&host("api.github.com")));
    assert!(!set.contains(&host("githubcopilot.com")));
    assert!(!set.contains(&host("gist.github.com")));
    assert!(!set.contains(&host("evilgithub.com")));
}

#[test]
fn slots_are_named_by_profile_and_role_and_each_role_is_listed_once() {
    assert_eq!(CLAUDE.slot_name(Role::ApiKey), "claude-api-key");
    assert_eq!(GITHUB.slot_name(Role::Access), "github-access");
    assert_eq!(CLAUDE.roles(), [Role::Access, Role::Refresh, Role::ApiKey]);
    assert_eq!(GITHUB.roles(), [Role::Access, Role::Refresh, Role::Copilot]);
    assert_eq!(GITHUB.slot_name(Role::Copilot), "github-copilot");
    // Two token hosts with two fields each, and the key.
    assert_eq!(CLAUDE.fields().count(), 5);
}

#[test]
fn profile_ids_and_paths_are_unique_and_well_formed() {
    let profiles = builtin();
    for (i, a) in profiles.iter().enumerate() {
        for b in &profiles[i + 1..] {
            assert_ne!(a.id, b.id);
        }
        assert!(a.id.bytes().all(|c| c.is_ascii_lowercase()));
        for e in a.endpoints {
            assert!(e.path.starts_with('/'));
            assert!(!e.fields.is_empty() || !e.request_fields.is_empty());
        }
    }
}

#[test]
fn every_host_a_field_names_is_one_the_profile_decrypts() {
    for profile in builtin() {
        let decrypted = profile.hosts();
        for (_, field) in profile.fields() {
            let Scope::Hosts(hosts) = field.scope else {
                continue;
            };
            assert!(!hosts.is_empty(), "{}: {}", profile.id, field.path);
            let set = profile.hosts_for(field.scope);
            for name in hosts {
                let probe = name
                    .strip_prefix("*.")
                    .map_or_else(|| (*name).to_owned(), |base| format!("anything.{base}"));
                assert!(set.contains(&host(&probe)), "{}: {name}", profile.id);
                assert!(decrypted.contains(&host(&probe)), "{}: {name}", profile.id);
            }
        }
    }
}
