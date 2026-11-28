// SPDX-License-Identifier: AGPL-3.0-or-later
//! The profile of GitHub.

use super::*;

/// GitHub (`gh auth login`, `copilot login`): the OAuth device flow and the web flow end at
/// `github.com/login/oauth/access_token`, answered as a form or as JSON by the request's `Accept`.
/// The Copilot extension's session token comes from `api.github.com/copilot_internal/v2/token`.
/// The tokens go to GitHub's API, to Git over HTTPS and to the host `copilot_internal/user` names
/// for the Copilot service.
pub const GITHUB: Profile = Profile {
    id: "github",
    name: "GitHub",
    endpoints: &[
        Endpoint {
            host: "github.com",
            method: Method::Post,
            path: "/login/oauth/access_token",
            request_fields: &["refresh_token"],
            fields: &[
                Field {
                    path: "access_token",
                    resource: None,
                    role: Role::Access,
                    prefixes: &["gho_", "ghu_"],
                    keep_last: 0,
                    stand_in: StandInKind::Shaped,
                    only_prefixed: false,
                    scope: Scope::Api,
                    audience: Audience::Any,
                    keep: Keep::Stored,
                },
                Field {
                    path: "refresh_token",
                    resource: None,
                    role: Role::Refresh,
                    prefixes: &["ghr_"],
                    keep_last: 0,
                    stand_in: StandInKind::Shaped,
                    only_prefixed: false,
                    scope: Scope::TokenEndpoint,
                    audience: Audience::Any,
                    keep: Keep::Stored,
                },
            ],
        },
        // The VS Code Copilot extension trades the stand-in `gho_` token (swapped back toward this
        // host) for a session token. The extension's request names no method, so it is a `GET`.
        Endpoint {
            host: "api.github.com",
            method: Method::Any,
            path: "/copilot_internal/v2/token",
            request_fields: &[],
            fields: &[Field {
                path: "token",
                resource: None,
                role: Role::Copilot,
                prefixes: &[],
                keep_last: 0,
                stand_in: StandInKind::Fields,
                only_prefixed: false,
                scope: Scope::Api,
                audience: Audience::Any,
                keep: Keep::Stored,
            }],
        },
    ],
    api_hosts: &[
        "github.com",
        "api.github.com",
        "uploads.github.com",
        "*.githubcopilot.com",
    ],
};
