// SPDX-License-Identifier: AGPL-3.0-or-later
//! The profile of Microsoft Entra.

use super::*;

/// The token fields Microsoft Entra answers with, the same at the v1 and v2 endpoints.
const ENTRA_TOKEN_FIELDS: &[Field] = &[
    // One access token field per resource: a tool holds tokens for several at once (`az` has one for
    // Azure Resource Manager and one for Graph), and a refresh of one must not replace the other.
    // Each is swapped only toward its own resource's hosts.
    Field {
        path: "access_token",
        resource: Some("arm"),
        role: Role::Access,
        // A token that is not a JWT (a personal account's) starts with `Ew`.
        prefixes: &["Ew"],
        keep_last: 0,
        stand_in: StandInKind::Jwt,
        only_prefixed: false,
        scope: Scope::Hosts(&["management.azure.com", "management.core.windows.net"]),
        audience: Audience::Named(&["management.azure.com", "management.core.windows.net"]),
        keep: Keep::Stored,
    },
    // Microsoft Graph, and the older Azure AD Graph that some tools still ask for.
    Field {
        path: "access_token",
        resource: Some("graph"),
        role: Role::Access,
        // A token that is not a JWT (a personal account's) starts with `Ew`.
        prefixes: &["Ew"],
        keep_last: 0,
        stand_in: StandInKind::Jwt,
        only_prefixed: false,
        scope: Scope::Hosts(&["graph.microsoft.com", "graph.windows.net"]),
        audience: Audience::Named(&[
            "graph.microsoft.com",
            "00000003-0000-0000-c000-000000000000",
            "graph.windows.net",
            "00000002-0000-0000-c000-000000000000",
        ]),
        keep: Keep::Stored,
    },
    Field {
        path: "access_token",
        resource: Some("devops"),
        role: Role::Access,
        // A token that is not a JWT (a personal account's) starts with `Ew`.
        prefixes: &["Ew"],
        keep_last: 0,
        stand_in: StandInKind::Jwt,
        only_prefixed: false,
        scope: Scope::Hosts(&["dev.azure.com", "*.dev.azure.com", "*.visualstudio.com"]),
        audience: Audience::Named(&[
            "499b84ac-1321-427f-aa17-267ca6975798",
            "dev.azure.com",
            "visualstudio.com",
        ]),
        keep: Keep::Stored,
    },
    Field {
        path: "access_token",
        resource: Some("vault"),
        role: Role::Access,
        // A token that is not a JWT (a personal account's) starts with `Ew`.
        prefixes: &["Ew"],
        keep_last: 0,
        stand_in: StandInKind::Jwt,
        only_prefixed: false,
        scope: Scope::Hosts(&["*.vault.azure.net"]),
        audience: Audience::Named(&["vault.azure.net"]),
        keep: Keep::Stored,
    },
    // A resource none of the above names (Storage, Azure SQL, Azure Data Explorer, an app's own
    // API): they share one slot, which keeps the last few stand-ins. Their hosts are not known in
    // advance (Storage's are under a public suffix, which no pattern may cover), so the stand-in is
    // swapped toward no API: only back toward the login hosts. A tool that sends it to such a
    // resource gets that resource's refusal, and the real token never leaves the host.
    Field {
        path: "access_token",
        resource: Some("other"),
        role: Role::Access,
        // A token that is not a JWT (a personal account's) starts with `Ew`.
        prefixes: &["Ew"],
        keep_last: 0,
        stand_in: StandInKind::Jwt,
        only_prefixed: false,
        scope: Scope::TokenEndpoint,
        audience: Audience::Unnamed,
        keep: Keep::Stored,
    },
    Field {
        path: "refresh_token",
        resource: None,
        role: Role::Refresh,
        // A Microsoft refresh token starts with its version and a dot, and is over a thousand
        // characters long.
        prefixes: &["0.", "1."],
        keep_last: 0,
        stand_in: StandInKind::ShapedLong,
        only_prefixed: false,
        scope: Scope::TokenEndpoint,
        audience: Audience::Any,
        keep: Keep::Stored,
    },
    // The tool reads who signed in and for how long from it; no service takes it as a credential.
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
];

/// The token endpoints of Entra's public cloud, `/{tenant}/oauth2/v2.0/token` and the older
/// `/{tenant}/oauth2/token`, on `login.microsoftonline.com` and its aliases, which answer the same
/// token requests (a tool or a library may be set to any of them).
const ENTRA_ENDPOINTS: &[Endpoint] = &[
    Endpoint {
        host: "login.microsoftonline.com",
        method: Method::Post,
        path: "/*/oauth2/v2.0/token",
        request_fields: &["refresh_token"],
        fields: ENTRA_TOKEN_FIELDS,
    },
    Endpoint {
        host: "login.microsoftonline.com",
        method: Method::Post,
        path: "/*/oauth2/token",
        request_fields: &["refresh_token"],
        fields: ENTRA_TOKEN_FIELDS,
    },
    Endpoint {
        host: "login.windows.net",
        method: Method::Post,
        path: "/*/oauth2/v2.0/token",
        request_fields: &["refresh_token"],
        fields: ENTRA_TOKEN_FIELDS,
    },
    Endpoint {
        host: "login.windows.net",
        method: Method::Post,
        path: "/*/oauth2/token",
        request_fields: &["refresh_token"],
        fields: ENTRA_TOKEN_FIELDS,
    },
    Endpoint {
        host: "login.microsoft.com",
        method: Method::Post,
        path: "/*/oauth2/v2.0/token",
        request_fields: &["refresh_token"],
        fields: ENTRA_TOKEN_FIELDS,
    },
    Endpoint {
        host: "login.microsoft.com",
        method: Method::Post,
        path: "/*/oauth2/token",
        request_fields: &["refresh_token"],
        fields: ENTRA_TOKEN_FIELDS,
    },
    Endpoint {
        host: "sts.windows.net",
        method: Method::Post,
        path: "/*/oauth2/v2.0/token",
        request_fields: &["refresh_token"],
        fields: ENTRA_TOKEN_FIELDS,
    },
    Endpoint {
        host: "sts.windows.net",
        method: Method::Post,
        path: "/*/oauth2/token",
        request_fields: &["refresh_token"],
        fields: ENTRA_TOKEN_FIELDS,
    },
];

/// Microsoft Entra (`az login`, Git Credential Manager for Azure `DevOps`, VS Code's Azure
/// sign-in): the OAuth token endpoint of the public cloud, `/{tenant}/oauth2/v2.0/token` and the
/// older `/{tenant}/oauth2/token`, on `login.microsoftonline.com` and its aliases, answered as JSON
/// for the code exchange, the device flow's poll and the refresh. The tenant is any one segment (an
/// id, a domain, `common`, `organizations`). Each access token goes to its own resource's hosts:
/// Azure Resource Manager, Graph, Azure `DevOps` or Key Vault. The national clouds (their own login
/// hosts) are not covered.
pub const ENTRA: Profile = Profile {
    id: "entra",
    name: "Microsoft Entra",
    endpoints: ENTRA_ENDPOINTS,
    api_hosts: &[
        "management.azure.com",
        "management.core.windows.net",
        "graph.microsoft.com",
        "graph.windows.net",
        "dev.azure.com",
        "*.dev.azure.com",
        "*.visualstudio.com",
        "*.vault.azure.net",
    ],
};
