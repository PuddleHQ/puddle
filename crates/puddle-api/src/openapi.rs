// SPDX-License-Identifier: GPL-3.0-or-later
//! The `OpenAPI` 3.1 contract (ADR 0004), generated from the routes and the [`crate::wire`]
//! types. Committed as `crates/puddle-api/openapi/openapi.json`, with the TypeScript types
//! generated from it; a test and the `openapi` gate fail when either is stale.

use utoipa::OpenApi as _;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityRequirement, SecurityScheme};
use utoipa::openapi::{ContentBuilder, OpenApi, Ref, RefOr, Response, ResponseBuilder};
use utoipa_axum::router::OpenApiRouter;

use crate::routes::{AppState, api_router, slow_router};

/// The contract's version. Bump the minor version for additive changes and the major version
/// for anything a generated client would break on.
pub const API_VERSION: &str = "0.31.0";

/// The security scheme's name in the spec.
const BEARER: &str = "bearer";

#[derive(utoipa::OpenApi)]
#[openapi(
    info(
        title = "Puddle",
        description = "Puddle's local API: pending requests, rules, audit, settings, consents and \
                       the event stream. Listens on 127.0.0.1 only. Every request needs \
                       `Authorization: Bearer <token>` (from the connection file), a `Host` of \
                       `127.0.0.1:<port>` or `localhost:<port>`, and no foreign `Origin`."
    ),
    components(schemas(
        puddle_types::Event,
        crate::events::Lagged,
        crate::error::ApiErrorBody,
        crate::error::ErrorCode,
        crate::wire::ConsentKind,
        crate::wire::AuditType,
        crate::wire::AuditOutcome,
        crate::wire::ConnectionOrigin,
        crate::wire::ConnectionOutcome,
        puddle_types::WorkspaceId
    )),
    tags(
        (name = "service", description = "Who is answering"),
        (name = "events", description = "The server-sent event stream"),
        (name = "auth", description = "Browser sign-in on a public address: the first visitor's owner account, passwords, devices and the sign-in methods"),
        (name = "pending", description = "Connections waiting for a decision"),
        (name = "rules", description = "Allow and deny rules"),
        (name = "audit", description = "The audit log"),
        (name = "settings", description = "Global and per-workspace settings"),
        (name = "consents", description = "What the user agreed to"),
        (name = "doctor", description = "The system check: what this computer needs to run workspaces, and the fix for what is missing"),
        (name = "first-run", description = "Whether the first-run flow has been through"),
        (name = "network", description = "How Puddle reaches the internet: proxy, sign-in, company roots"),
        (name = "identities", description = "Git identities (author and credentials) and each workspace's identities, repository table and push and pull switches"),
        (name = "workspaces", description = "Workspaces: their own disk with the repositories Puddle manages for them, and the sandbox that runs them"),
        (name = "ports", description = "A workspace's ports: what listens in it, forwards to this computer's loopback, pinned forwards"),
        (name = "environment", description = "Environment variables and secrets, global and per workspace; a secret's value is write-only"),
        (name = "interception", description = "Decrypting every host: a workspace's switch and the names that are never decrypted"),
        (name = "shell-init", description = "A workspace's shell init: the script its login shells source; the host's copy is the only source of truth"),
        (name = "logins", description = "The logins Puddle kept for a workspace: listing them and forgetting one"),
        (name = "orphan-volumes", description = "Workspace disks the workspace list does not name, and restoring one as a workspace"),
        (name = "start-folder", description = "Where a workspace's new terminals land and what its editor opens")
    )
)]
struct ApiDoc;

/// The spec.
#[must_use]
pub fn openapi() -> OpenApi {
    let mut doc = OpenApiRouter::<AppState>::with_openapi(ApiDoc::openapi())
        .merge(api_router())
        .merge(slow_router())
        .into_openapi();
    API_VERSION.clone_into(&mut doc.info.version);
    doc.components
        .get_or_insert_with(Default::default)
        .add_security_scheme(
            BEARER,
            SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
        );
    doc.security = Some(vec![SecurityRequirement::new(BEARER, Vec::<String>::new())]);
    add_guard_responses(&mut doc);
    doc
}

/// The spec as pretty-printed JSON with a trailing newline: the committed file's exact bytes.
#[must_use]
pub fn openapi_json() -> String {
    let mut json = openapi()
        .to_pretty_json()
        .unwrap_or_else(|err| format!("{{\"error\":\"{err}\"}}"));
    json.push('\n');
    json
}

fn error_response(description: &str) -> RefOr<Response> {
    RefOr::T(
        ResponseBuilder::new()
            .description(description)
            .content(
                "application/json",
                ContentBuilder::new()
                    .schema(Some(Ref::from_schema_name("ApiErrorBody")))
                    .build(),
            )
            .build(),
    )
}

/// The guard's refusals and the internal error apply to every operation; listing them once here
/// keeps the handlers' annotations to what is specific to them.
fn add_guard_responses(doc: &mut OpenApi) {
    let common = [
        ("401", "missing or wrong bearer token"),
        ("403", "forbidden origin"),
        ("421", "Host is not the API's own address"),
        ("500", "Puddle failed; see its log"),
    ];
    for item in doc.paths.paths.values_mut() {
        let operations = [
            &mut item.get,
            &mut item.put,
            &mut item.post,
            &mut item.delete,
            &mut item.patch,
        ];
        for operation in operations.into_iter().flatten() {
            for (status, description) in common {
                operation
                    .responses
                    .responses
                    .entry(status.to_owned())
                    .or_insert_with(|| error_response(description));
            }
        }
    }
}

#[cfg(test)]
mod tests;
