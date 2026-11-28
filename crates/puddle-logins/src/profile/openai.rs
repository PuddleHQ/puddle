// SPDX-License-Identifier: AGPL-3.0-or-later
//! The profile of `OpenAI`'s Codex.

use super::*;

/// `OpenAI`'s Codex (`codex login` with a `ChatGPT` account): the OAuth code exchange, the refresh and
/// the token exchange that trades the identity token for an API key all go to
/// `auth.openai.com/oauth/token`.
///
/// - The code exchange and the refresh answer with `id_token`, `access_token` and `refresh_token`.
///   Both JWTs are read by Codex (who signed in, which plan, when the token ends), so their stand-ins
///   keep header and payload.
/// - The token exchange answers with an API key (`sk-...`) in the same `access_token` field, so
///   that field is listed twice: the API key first, because it only takes a token with its prefix.
/// - The refresh sends `refresh_token`, the token exchange sends the identity token as
///   `subject_token`.
/// - The tokens go to the API (`api.openai.com`) and to `ChatGPT`'s backend (`chatgpt.com`).
///
/// The hosts and the shape of the tokens are from the Codex source and public descriptions of the
/// login, not from a recording.
pub const OPENAI: Profile = Profile {
    id: "openai",
    name: "OpenAI Codex",
    endpoints: &[Endpoint {
        host: "auth.openai.com",
        method: Method::Post,
        path: "/oauth/token",
        request_fields: &["refresh_token", "subject_token"],
        fields: &[
            Field {
                path: "access_token",
                resource: None,
                role: Role::ApiKey,
                prefixes: &["sk-"],
                keep_last: 0,
                stand_in: StandInKind::Shaped,
                only_prefixed: true,
                scope: Scope::Api,
                audience: Audience::Any,
                keep: Keep::Stored,
            },
            Field {
                path: "access_token",
                resource: None,
                role: Role::Access,
                prefixes: &[],
                keep_last: 0,
                stand_in: StandInKind::Jwt,
                only_prefixed: false,
                scope: Scope::Api,
                audience: Audience::Any,
                keep: Keep::Stored,
            },
            Field {
                path: "id_token",
                resource: None,
                role: Role::IdToken,
                prefixes: &[],
                keep_last: 0,
                stand_in: StandInKind::Jwt,
                only_prefixed: false,
                scope: Scope::TokenEndpoint,
                audience: Audience::Any,
                keep: Keep::Stored,
            },
            Field {
                path: "refresh_token",
                resource: None,
                role: Role::Refresh,
                prefixes: &["rt_"],
                keep_last: 0,
                stand_in: StandInKind::Shaped,
                only_prefixed: false,
                scope: Scope::TokenEndpoint,
                audience: Audience::Any,
                keep: Keep::Stored,
            },
        ],
    }],
    api_hosts: &["api.openai.com", "chatgpt.com"],
};
