// SPDX-License-Identifier: AGPL-3.0-or-later
//! The profile of Claude Code.

use super::*;

/// The token fields Claude Code's token endpoints answer with, the same on every host that serves
/// the exchange.
const CLAUDE_TOKEN_FIELDS: &[Field] = &[
    Field {
        path: "access_token",
        resource: None,
        role: Role::Access,
        prefixes: &["sk-ant-oat01-"],
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
        prefixes: &["sk-ant-ort01-"],
        keep_last: 0,
        stand_in: StandInKind::Shaped,
        only_prefixed: false,
        scope: Scope::TokenEndpoint,
        audience: Audience::Any,
        keep: Keep::Stored,
    },
];

/// Claude Code (`claude auth login`, `/login`): the OAuth code exchange and refresh at
/// `platform.claude.com`, JSON both ways, and the API key the login creates. Versions up to 2.1.5
/// use `console.anthropic.com` for the same exchange.
pub const CLAUDE: Profile = Profile {
    id: "claude",
    name: "Claude Code",
    endpoints: &[
        Endpoint {
            host: "platform.claude.com",
            method: Method::Post,
            path: "/v1/oauth/token",
            request_fields: &["refresh_token"],
            fields: CLAUDE_TOKEN_FIELDS,
        },
        Endpoint {
            host: "console.anthropic.com",
            method: Method::Post,
            path: "/v1/oauth/token",
            request_fields: &["refresh_token"],
            fields: CLAUDE_TOKEN_FIELDS,
        },
        Endpoint {
            host: "api.anthropic.com",
            method: Method::Post,
            path: "/api/oauth/claude_cli/create_api_key",
            request_fields: &[],
            fields: &[Field {
                path: "raw_key",
                resource: None,
                role: Role::ApiKey,
                prefixes: &["sk-ant-api03-"],
                // The CLI keeps the last 20 characters of the key it was given.
                keep_last: 20,
                stand_in: StandInKind::Shaped,
                only_prefixed: false,
                scope: Scope::Api,
                audience: Audience::Any,
                keep: Keep::Stored,
            }],
        },
    ],
    api_hosts: &[
        "api.anthropic.com",
        "claude.ai",
        "console.anthropic.com",
        "mcp-proxy.anthropic.com",
    ],
};
