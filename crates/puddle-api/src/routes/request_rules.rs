// SPDX-License-Identifier: GPL-3.0-or-later
//! Rules on the method and path of a request: they only ever refuse.
//!
//! A change reaches a running workspace at once: the store announces it and the host reads the
//! rules again, with no restart.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use puddle_store::{Actor, NewRequestRule};
use puddle_types::{Methods, PathPattern, WorkspaceName};
use serde::Deserialize;

use crate::ApiErrorBody;
use crate::error::{ApiError, blocking};
use crate::extract::{Path, Query};
use crate::routes::AppState;
use crate::routes::interception::pattern;
use crate::wire::{NewRequestRuleRequest, RequestRule, RequestRuleList};

/// Which rules to list.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequestRuleQuery {
    workspace: Option<String>,
}

/// The rules on method and path: all of them, or the ones that apply to one workspace (the
/// global ones and its own).
#[utoipa::path(
    get,
    path = "/api/request-rules",
    tag = "rules",
    params(("workspace" = Option<String>, Query, description = "only the rules that apply to this workspace")),
    responses(
        (status = OK, description = "the rules, in id order", body = RequestRuleList),
        (status = UNPROCESSABLE_ENTITY, description = "not a workspace name", body = ApiErrorBody)
    )
)]
pub(crate) async fn list_request_rules(
    State(state): State<AppState>,
    Query(query): Query<RequestRuleQuery>,
) -> Result<Json<RequestRuleList>, ApiError> {
    let workspace = query
        .workspace
        .map(|raw| {
            WorkspaceName::new(&raw).map_err(|err| ApiError::invalid(format!("workspace: {err}")))
        })
        .transpose()?;
    let rules = blocking(move || Ok(state.store.request_rules(workspace.as_ref())?)).await?;
    Ok(Json(RequestRuleList {
        rules: rules.into_iter().map(RequestRule::from).collect(),
    }))
}

/// Makes a rule that refuses the named methods of the named paths on a host. The host is
/// decrypted from now on whatever the workspace's other settings say, so the rule can read its
/// requests; a tunnel to another port of the host is refused.
#[utoipa::path(
    post,
    path = "/api/request-rules",
    tag = "rules",
    request_body = NewRequestRuleRequest,
    responses(
        (status = CREATED, description = "the new rule", body = RequestRule),
        (status = UNPROCESSABLE_ENTITY, description = "invalid host, method or path, or more than 500 rules in the scope", body = ApiErrorBody)
    )
)]
pub(crate) async fn create_request_rule(
    State(state): State<AppState>,
    crate::extract::Json(body): crate::extract::Json<NewRequestRuleRequest>,
) -> Result<(StatusCode, Json<RequestRule>), ApiError> {
    let host = pattern(&body.host)?;
    let methods = Methods::parse(body.methods.iter().map(String::as_str))
        .map_err(|err| ApiError::invalid(format!("methods: {err}")))?;
    let path =
        PathPattern::parse(&body.path).map_err(|err| ApiError::invalid(format!("path: {err}")))?;
    let new = NewRequestRule {
        workspace: body.workspace,
        host,
        methods,
        path,
        created_by: Actor::Api,
    };
    let rule = blocking(move || Ok(state.store.add_request_rule(&new)?)).await?;
    Ok((StatusCode::CREATED, Json(rule.into())))
}

/// Deletes a rule on method and path.
#[utoipa::path(
    delete,
    path = "/api/request-rules/{id}",
    tag = "rules",
    params(("id" = i64, Path, description = "rule id")),
    responses(
        (status = OK, description = "the deleted rule", body = RequestRule),
        (status = NOT_FOUND, description = "no such rule", body = ApiErrorBody)
    )
)]
pub(crate) async fn delete_request_rule(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<RequestRule>, ApiError> {
    let rule = blocking(move || Ok(state.store.delete_request_rule(id, Actor::Api)?)).await?;
    Ok(Json(rule.into()))
}
