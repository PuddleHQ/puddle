// SPDX-License-Identifier: GPL-3.0-or-later
//! The routes. Each handler carries its `#[utoipa::path]`, and [`api_router`] registers routes
//! and spec together (`utoipa-axum`), so the published spec can't drift from what is served.

use std::sync::Arc;

use puddle_netpolicy::CompanySignals;
use puddle_secrets::SecretStore;
use puddle_store::{
    Actor, Clock, NewWorkspaceOp, Store, UserId, WorkspaceOpKind, WorkspaceOpOutcome,
};
use tokio::sync::{Mutex, watch};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::authz::Authorizer;
use crate::credentials::CredentialService;
use crate::dev_cert::DevCertService;
use crate::doctor::DoctorService;
use crate::events::EventHub;
use crate::learned::LearnedService;
use crate::network_health::NetworkHealthService;
use crate::ports::PortsService;
use crate::proxy_sign_in::ProxySignInService;
use crate::repos::RepoService;
use crate::settings::SettingsRepo;
use crate::signin::SignIn;
use crate::terminals::{Registry, TerminalService};
use crate::workspaces::WorkspaceService;

mod app_icon;
mod audit;
pub(crate) mod auth;
mod credentials;
mod doctor;
mod environment;
mod events;
mod first_run;
mod identities;
mod interception;
mod logins;
mod meta;
mod network_health;
mod orphan_volumes;
mod pending;
mod ports;
mod problems;
mod proxy_sign_in;
mod repos;
mod repositories;
mod repository_hosts;
mod request_rules;
mod rule_sets;
mod rules;
pub(crate) mod settings;
mod shell_init;
mod ssh_agent;
mod start_folder;
mod terminals;
mod workspaces;

/// Writes a `workspace_op` record of `user` for a change a request made (all of them reach Puddle
/// through the API, so the actor is `api`). The change has happened, so a record that cannot be written
/// is logged and dropped rather than turned into an error the user would read as "not done".
pub(crate) fn record_op(
    store: &Store,
    user: UserId,
    op: WorkspaceOpKind,
    workspace: Option<&puddle_types::WorkspaceId>,
    detail: Option<String>,
) {
    let new = NewWorkspaceOp {
        op,
        workspace: workspace.cloned(),
        user: Some(user),
        outcome: WorkspaceOpOutcome::Succeeded,
        actor: Actor::Api,
        detail,
    };
    store.note_workspace_op(&new);
}

/// What the handlers share.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) store: Arc<Store>,
    /// Who may do what; every route asks it before its handler runs.
    pub(crate) authorizer: Arc<dyn Authorizer>,
    pub(crate) settings: Arc<dyn SettingsRepo>,
    /// Serialises settings read-modify-write cycles.
    pub(crate) settings_lock: Arc<Mutex<()>>,
    pub(crate) events: Arc<EventHub>,
    pub(crate) problems: Arc<puddle_types::Problems>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) workspaces: Arc<dyn WorkspaceService>,
    pub(crate) network_health: Arc<dyn NetworkHealthService>,
    pub(crate) credentials: Arc<dyn CredentialService>,
    /// Where the values of environment secrets are kept.
    pub(crate) secrets: Arc<dyn SecretStore>,
    pub(crate) repos: Arc<dyn RepoService>,
    pub(crate) doctor: Arc<dyn DoctorService>,
    /// Where the development certificate stands (the first-run flow's certificates step).
    pub(crate) dev_cert: Arc<dyn DevCertService>,
    pub(crate) ports: Arc<dyn PortsService>,
    pub(crate) learned: Arc<dyn LearnedService>,
    pub(crate) terminal_service: Arc<dyn TerminalService>,
    /// The tickets handed out and the terminals open.
    pub(crate) terminals: Arc<Registry>,
    pub(crate) proxy_sign_ins: Arc<dyn ProxySignInService>,
    /// What the host reads of the company network, for the `blocked_by` of a pending request.
    pub(crate) company_signals: Arc<dyn CompanySignals>,
    /// Browser sign-in; `None` when the host gave the API no accounts to sign in to.
    pub(crate) sign_in: Option<Arc<SignIn>>,
    /// The build of the app this server serves (`GET /api/health`), `None` when it serves none.
    pub(crate) ui_build_id: Option<String>,
    /// Becomes `true` when the server shuts down; ends SSE streams.
    pub(crate) shutdown: watch::Receiver<bool>,
}

/// Every documented route.
pub(crate) fn api_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(meta::health))
        .routes(routes!(problems::list_problems))
        .routes(routes!(events::events))
        .routes(routes!(network_health::network_health))
        .routes(routes!(
            proxy_sign_in::list_proxy_sign_ins,
            proxy_sign_in::put_proxy_sign_in
        ))
        .routes(routes!(proxy_sign_in::delete_proxy_sign_in))
        .routes(routes!(pending::list_pending))
        .routes(routes!(pending::inbox))
        .routes(routes!(pending::get_pending))
        .routes(routes!(pending::approve))
        .routes(routes!(pending::deny))
        .merge(rules_router())
        .merge(auth_router())
        .routes(routes!(audit::audit))
        .routes(routes!(settings::get_global, settings::put_global))
        .routes(routes!(
            settings::get_workspace_settings,
            settings::put_workspace_settings
        ))
        .routes(routes!(
            app_icon::list_app_icons,
            app_icon::delete_custom_icon
        ))
        .merge(app_icon_upload_router())
        .routes(routes!(settings::get_consents))
        .routes(routes!(settings::put_consent))
        .routes(routes!(first_run::get_first_run, first_run::put_first_run))
        .routes(routes!(
            workspaces::list_workspaces,
            workspaces::create_workspace
        ))
        .routes(routes!(workspaces::lookup_workspace))
        .routes(routes!(
            workspaces::get_workspace,
            workspaces::rename_workspace,
            workspaces::delete_workspace
        ))
        .routes(routes!(start_folder::put_start_folder))
        .routes(routes!(workspaces::put_session_user))
        .routes(routes!(workspaces::start_workspace))
        .routes(routes!(workspaces::stop_workspace))
        .routes(routes!(workspaces::reclaim_workspace))
        .routes(routes!(workspaces::reset_root_disk))
        .routes(routes!(workspaces::grow_root_disk))
        .routes(routes!(workspaces::delete_check))
        .routes(routes!(workspaces::attach_workspace))
        .routes(routes!(repository_hosts::repository_hosts))
        .routes(routes!(repositories::list_checkouts))
        .routes(routes!(repositories::add_repository))
        .routes(routes!(repositories::clone_again))
        .routes(routes!(repositories::remove_repository))
        .routes(routes!(
            identities::list_identities,
            identities::create_identity
        ))
        .routes(routes!(identities::reorder_identities))
        .routes(routes!(
            identities::get_git_defaults,
            identities::set_git_defaults
        ))
        .routes(routes!(
            identities::get_identity,
            identities::update_identity,
            identities::delete_identity
        ))
        .routes(routes!(identities::set_default_identity))
        .routes(routes!(credentials::found_accounts))
        .routes(routes!(credentials::check_credential))
        .routes(routes!(credentials::store_token))
        .routes(routes!(credentials::forget_token))
        .routes(routes!(credentials::sign_in))
        .merge(environment_router())
        .routes(routes!(ports::list_ports))
        .routes(routes!(ports::forward_port, ports::release_port))
        .merge(terminals_router())
        .routes(routes!(repos::credential_profile))
        .routes(routes!(repos::list_repos))
        .routes(routes!(repos::refresh_repos))
        .routes(routes!(identities::get_workspace_git))
        .routes(routes!(
            identities::set_workspace_identities,
            identities::attach_identity
        ))
        .routes(routes!(identities::detach_identity))
        .routes(routes!(identities::set_git_switches))
        .routes(routes!(identities::add_git_repo))
        .routes(routes!(
            identities::set_git_repo,
            identities::remove_git_repo
        ))
}

/// Browser sign-in, devices and the sign-in methods, in a router of their own like the rules.
#[inline(never)]
fn auth_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(auth::status))
        .routes(routes!(auth::setup))
        .routes(routes!(auth::password_sign_in))
        .routes(routes!(auth::invite))
        .routes(routes!(auth::refresh))
        .routes(routes!(auth::sign_out))
        .routes(routes!(auth::change_password))
        .routes(routes!(auth::list_devices))
        .routes(routes!(auth::revoke_device))
        .routes(routes!(auth::get_methods, auth::put_methods))
}

/// The terminals, in a router of their own like the rules.
#[inline(never)]
fn terminals_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(terminals::create_terminal))
        .routes(routes!(terminals::terminal_socket))
}

/// The rules: allow and deny rules, rules on method and path, and rule sets. A router of their own
/// so the stack frame that builds the whole API stays small (the host starts on a 1 MiB stack).
#[inline(never)]
fn rules_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(rules::list_rules, rules::create_rule))
        .routes(routes!(rules::delete_rule))
        .routes(routes!(rules::set_rule_expiry))
        .routes(routes!(
            request_rules::list_request_rules,
            request_rules::create_request_rule
        ))
        .routes(routes!(request_rules::delete_request_rule))
        .routes(routes!(
            rule_sets::list_rule_sets,
            rule_sets::create_rule_set
        ))
        .routes(routes!(
            rule_sets::update_rule_set,
            rule_sets::delete_rule_set
        ))
        .routes(routes!(rule_sets::switch_rule_set))
}

/// The upload of the user's own app icon: the one route whose body may be a few megabytes.
fn app_icon_upload_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(app_icon::put_custom_icon))
        .layer(axum::extract::DefaultBodyLimit::max(
            puddle_appicon::MAX_UPLOAD_BYTES,
        ))
}

/// What a workspace keeps beside its Git settings: its environment, its shell init and the logins
/// kept for it.
fn environment_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(environment::list_global_env))
        .routes(routes!(
            environment::put_global_env,
            environment::delete_global_env
        ))
        .routes(routes!(environment::list_workspace_env))
        .routes(routes!(
            environment::put_workspace_env,
            environment::delete_workspace_env
        ))
        .routes(routes!(
            shell_init::get_shell_init,
            shell_init::put_shell_init,
            shell_init::delete_shell_init
        ))
        .routes(routes!(
            interception::get_interception,
            interception::put_interception
        ))
        .routes(routes!(
            interception::get_interception_settings,
            interception::put_interception_settings
        ))
        .routes(routes!(
            interception::get_global_never_decrypt,
            interception::put_global_never_decrypt
        ))
        .routes(routes!(
            interception::list_learned,
            interception::clear_learned
        ))
        .routes(routes!(interception::forget_learned))
        .routes(routes!(interception::make_permanent))
        .routes(routes!(
            interception::get_learned_limits,
            interception::put_learned_limits
        ))
        .routes(routes!(ssh_agent::get_ssh_agent))
        .routes(routes!(
            ssh_agent::put_ssh_agent_host,
            ssh_agent::delete_ssh_agent_host
        ))
        .routes(routes!(logins::list_logins))
        .routes(routes!(logins::forget_login))
        .routes(routes!(orphan_volumes::list_orphan_volumes))
        .routes(routes!(orphan_volumes::restore_orphan_volume))
}

/// The routes that may take far longer than the rest (the system check boots a test machine, the
/// certificate's trust waits for the user), so they get their own, longer time limit.
pub(crate) fn slow_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(doctor::doctor))
        .routes(routes!(first_run::trust_dev_certificate))
}
