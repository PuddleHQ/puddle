// SPDX-License-Identifier: AGPL-3.0-or-later
//! The profile of Docker Hub.

use super::*;

/// The token fields of Docker's login host (Auth0): the device flow's poll and a refresh answer with
/// the same three. The access token is a JWT whose claims `docker` reads (`username` under
/// `https://hub.docker.com`) and verifies against nothing.
const DOCKER_TOKEN_FIELDS: &[Field] = &[
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
        prefixes: &[],
        keep_last: 0,
        stand_in: StandInKind::Shaped,
        only_prefixed: false,
        scope: Scope::TokenEndpoint,
        audience: Audience::Any,
        keep: Keep::Stored,
    },
];

/// A personal or organization access token that Docker Hub's API creates and shows once: in the
/// answer's `token` field, kept under its own slot (`resource`) so it never replaces the token the
/// CLI's login made. A second token of the same kind gets the same stand-in, with the newer token
/// behind it.
const DOCKER_CREATED_PAT: &[Field] = &[Field {
    path: "token",
    resource: Some("created"),
    role: Role::ApiKey,
    prefixes: &["dckr_pat_"],
    keep_last: 0,
    stand_in: StandInKind::Shaped,
    only_prefixed: false,
    scope: Scope::Api,
    audience: Audience::Any,
    keep: Keep::Stored,
}];

/// An organization's access token, as [`DOCKER_CREATED_PAT`].
const DOCKER_CREATED_OAT: &[Field] = &[Field {
    path: "token",
    resource: Some("org"),
    role: Role::ApiKey,
    prefixes: &["dckr_oat_"],
    keep_last: 0,
    stand_in: StandInKind::Shaped,
    only_prefixed: false,
    scope: Scope::Api,
    audience: Audience::Any,
    keep: Keep::Stored,
}];

/// The registry's bearer token, which `auth.docker.io` mints for one repository and a few minutes
/// (the answer has it twice, as `token` and `access_token`). It goes to the registry only.
const DOCKER_REGISTRY_FIELDS: &[Field] = &[
    Field {
        path: "token",
        resource: Some("registry"),
        role: Role::Access,
        prefixes: &[],
        keep_last: 0,
        stand_in: StandInKind::Jwt,
        only_prefixed: false,
        scope: Scope::Hosts(&["registry-1.docker.io"]),
        audience: Audience::Any,
        keep: Keep::Memory,
    },
    Field {
        path: "access_token",
        resource: Some("registry"),
        role: Role::Access,
        prefixes: &[],
        keep_last: 0,
        stand_in: StandInKind::Jwt,
        only_prefixed: false,
        scope: Scope::Hosts(&["registry-1.docker.io"]),
        audience: Audience::Any,
        keep: Keep::Memory,
    },
];

/// Docker Hub (`docker login` through the browser, the device flow): the login host
/// `login.docker.com` answers the poll with an access token (a JWT), an identity token (a JWT) and a
/// refresh token; `docker logout` sends the refresh token back to `/oauth/revoke`. The CLI then
/// trades the access token for a personal access token at
/// `hub.docker.com/v2/access-tokens/desktop-generate` (answer `{"data":{"token":"dckr_pat_..."}}`),
/// stores it, and sends it as the password of a `Basic` credential to `auth.docker.io`, which
/// answers with a registry bearer token for the registry.
///
/// Every answer in this chain that carries a credential is read, so none reaches the workspace:
/// - the registry's bearer token from `auth.docker.io/token` (asked with `GET`, or with `POST` in
///   the `OAuth2` form) is held in memory ([`Keep::Memory`]), because it is minted for every pull,
///   lasts minutes, and an anonymous pull gets one too;
/// - an access token that a request to Docker Hub's API creates with the access token's stand-in
///   (`POST /v2/access-tokens`, and `POST /v2/orgs/{org}/access-tokens` for an organization's) is
///   kept like the login's own.
///
/// A password typed to `docker login -u` is not a login answer and is not covered here. The hosts
/// and fields are from the Docker CLI source (`internal/oauth`) and Docker Hub's published API
/// description, and the routes and the token answer's shape were checked against the live service;
/// none is from a recorded sign-in.
pub const DOCKER: Profile = Profile {
    id: "docker",
    name: "Docker Hub",
    endpoints: &[
        Endpoint {
            host: "login.docker.com",
            method: Method::Post,
            path: "/oauth/token",
            request_fields: &["refresh_token"],
            fields: DOCKER_TOKEN_FIELDS,
        },
        // The answer is empty and not read; the endpoint is listed for the request: the refresh
        // token's stand-in is swapped in the `token` field.
        Endpoint {
            host: "login.docker.com",
            method: Method::Post,
            path: "/oauth/revoke",
            request_fields: &["token"],
            fields: &[],
        },
        Endpoint {
            host: "hub.docker.com",
            method: Method::Post,
            path: "/v2/access-tokens/desktop-generate",
            request_fields: &[],
            fields: &[Field {
                path: "data.token",
                resource: None,
                role: Role::ApiKey,
                prefixes: &["dckr_pat_"],
                keep_last: 0,
                stand_in: StandInKind::Shaped,
                only_prefixed: false,
                scope: Scope::Api,
                audience: Audience::Any,
                keep: Keep::Stored,
            }],
        },
        Endpoint {
            host: "hub.docker.com",
            method: Method::Post,
            path: "/v2/access-tokens",
            request_fields: &[],
            fields: DOCKER_CREATED_PAT,
        },
        Endpoint {
            host: "hub.docker.com",
            method: Method::Post,
            path: "/v2/orgs/*/access-tokens",
            request_fields: &[],
            fields: DOCKER_CREATED_OAT,
        },
        // No request field is swapped: the token is asked for with a `Basic` credential in a
        // header, which is swapped as any header is.
        Endpoint {
            host: "auth.docker.io",
            method: Method::Any,
            path: "/token",
            request_fields: &[],
            fields: DOCKER_REGISTRY_FIELDS,
        },
    ],
    api_hosts: &["auth.docker.io", "registry-1.docker.io"],
};
