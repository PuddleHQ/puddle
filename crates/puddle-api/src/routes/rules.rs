// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rules: list, create, edit, delete, change expiry, switch on or off (docs/spec/rules.md §1–2, R-65).

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use puddle_store::Actor;
use puddle_types::RuleId;

use crate::ApiErrorBody;
use crate::authz::{self, Principal};
use crate::error::{ApiError, blocking};
use crate::extract::Path;
use crate::routes::{AppState, pending};
use crate::wire::{
    NewRuleRequest, Rule, RuleEnabledRequest, RuleExpiryRequest, RuleList, UndoOutcome,
    UpdateRuleRequest,
};

/// Every rule, including expired ones the sweeper hasn't removed yet.
#[utoipa::path(
    get,
    path = "/api/rules",
    tag = "rules",
    responses((status = OK, description = "the rules", body = RuleList))
)]
pub(crate) async fn list_rules(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<RuleList>, ApiError> {
    Ok(Json(RuleList {
        rules: state
            .store
            .rules(principal.user)
            .into_iter()
            .map(Rule::from_store)
            .collect::<Result<_, _>>()?,
    }))
}

/// Creates a rule directly. Open requests it now decides are closed the same way.
#[utoipa::path(
    post,
    path = "/api/rules",
    tag = "rules",
    request_body = NewRuleRequest,
    responses(
        (status = CREATED, description = "the new rule", body = Rule),
        (status = UNPROCESSABLE_ENTITY, description = "invalid pattern or expiry", body = ApiErrorBody)
    )
)]
pub(crate) async fn create_rule(
    State(state): State<AppState>,
    principal: Principal,
    crate::extract::Json(body): crate::extract::Json<NewRuleRequest>,
) -> Result<(StatusCode, Json<Rule>), ApiError> {
    let (new, precedence) = body.into_new_rule()?;
    let (action, resource) = authz::rule_scope(new.scope.workspace());
    authz::check(&state, principal, action, resource).await?;
    let rule = blocking(move || {
        let user = state.store.as_user(principal.user);
        Ok(match precedence {
            Some(precedence) => user.add_rule_with_precedence(&new, precedence)?,
            None => user.add_rule(&new)?,
        })
    })
    .await?;
    Ok((StatusCode::CREATED, Json(Rule::from_store(rule)?)))
}

/// Edits a rule in place: its host pattern, effect, scope and expiry (and its precedence when the
/// body names one). The rule keeps its id, creator, creation time and switch. Open requests it
/// now decides are closed the same way as for a new rule.
#[utoipa::path(
    put,
    path = "/api/rules/{id}",
    tag = "rules",
    params(("id" = i64, Path, description = "rule id")),
    request_body = UpdateRuleRequest,
    responses(
        (status = OK, description = "the edited rule", body = Rule),
        (status = NOT_FOUND, description = "no such rule, workspace or rule set", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "invalid pattern, expiry or precedence", body = ApiErrorBody)
    )
)]
pub(crate) async fn update_rule(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<i64>,
    crate::extract::Json(body): crate::extract::Json<UpdateRuleRequest>,
) -> Result<Json<Rule>, ApiError> {
    let edit = body.into_edit()?;
    // The route's own check covered the rule as it is; the scope it moves to needs its own.
    let (action, resource) = authz::rule_scope(edit.scope.workspace());
    authz::check(&state, principal, action, resource).await?;
    let rule =
        blocking(move || Ok(state.store.update_rule(RuleId(id), &edit, Actor::Api)?)).await?;
    Ok(Json(Rule::from_store(rule)?))
}

/// Deletes a rule.
#[utoipa::path(
    delete,
    path = "/api/rules/{id}",
    tag = "rules",
    params(("id" = i64, Path, description = "rule id")),
    responses(
        (status = OK, description = "the deleted rule", body = Rule),
        (status = NOT_FOUND, description = "no such rule", body = ApiErrorBody)
    )
)]
pub(crate) async fn delete_rule(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Rule>, ApiError> {
    let rule = blocking(move || Ok(state.store.delete_rule(RuleId(id), Actor::Api)?)).await?;
    Ok(Json(Rule::from_store(rule)?))
}

/// Takes back a decision made from the inbox: deletes the rule and puts the requests it closed
/// back in the inbox, in their old place.
#[utoipa::path(
    post,
    path = "/api/rules/{id}/undo",
    tag = "rules",
    params(("id" = i64, Path, description = "rule id")),
    responses(
        (status = OK, description = "the deleted rule and the requests that are open again", body = UndoOutcome),
        (status = NOT_FOUND, description = "no such rule", body = ApiErrorBody)
    )
)]
pub(crate) async fn undo_decision(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<UndoOutcome>, ApiError> {
    let (rule, reopened) = {
        let state = state.clone();
        blocking(move || Ok(state.store.undo_decision(RuleId(id), Actor::Api)?)).await?
    };
    Ok(Json(UndoOutcome {
        rule: Rule::from_store(rule)?,
        reopened: pending::present(&state, reopened).await?,
    }))
}

/// Changes a rule's expiry, or makes it permanent with `null`.
#[utoipa::path(
    put,
    path = "/api/rules/{id}/expiry",
    tag = "rules",
    params(("id" = i64, Path, description = "rule id")),
    request_body = RuleExpiryRequest,
    responses(
        (status = OK, description = "the changed rule", body = Rule),
        (status = NOT_FOUND, description = "no such rule", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "expiry not in the future", body = ApiErrorBody)
    )
)]
pub(crate) async fn set_rule_expiry(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    crate::extract::Json(body): crate::extract::Json<RuleExpiryRequest>,
) -> Result<Json<Rule>, ApiError> {
    let rule = blocking(move || {
        Ok(state
            .store
            .set_rule_expiry(RuleId(id), body.expires_at, Actor::Api)?)
    })
    .await?;
    Ok(Json(Rule::from_store(rule)?))
}

/// Switches a rule on or off. A rule that is off is kept and never matches; switching one on
/// closes the open requests it now decides.
#[utoipa::path(
    put,
    path = "/api/rules/{id}/enabled",
    tag = "rules",
    params(("id" = i64, Path, description = "rule id")),
    request_body = RuleEnabledRequest,
    responses(
        (status = OK, description = "the changed rule", body = Rule),
        (status = NOT_FOUND, description = "no such rule", body = ApiErrorBody)
    )
)]
pub(crate) async fn set_rule_enabled(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    crate::extract::Json(body): crate::extract::Json<RuleEnabledRequest>,
) -> Result<Json<Rule>, ApiError> {
    let rule = blocking(move || {
        Ok(state
            .store
            .set_rule_enabled(RuleId(id), body.enabled, Actor::Api)?)
    })
    .await?;
    Ok(Json(Rule::from_store(rule)?))
}
