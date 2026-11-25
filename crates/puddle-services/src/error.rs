// SPDX-License-Identifier: AGPL-3.0-or-later
//! Error responses: one JSON shape for every refusal and failure.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use puddle_settings::SettingsError;
use puddle_store::StoreError;
use serde::Serialize;
use utoipa::ToSchema;

use crate::backup::BackupServiceError;
use crate::credentials::CredentialsError;
use crate::doctor::DoctorError;
use crate::network_health::NetworkHealthError;
use crate::ports::PortsServiceError;
use crate::proxy_sign_in::ProxySignInError;
use crate::repos::ReposError;
use crate::restore::RestoreServiceError;
use crate::terminals::TerminalError;
use crate::workspaces::WorkspaceError;

/// What went wrong, as a stable code a client can switch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorCode {
    /// The `Host` header isn't the API's own address (421).
    MisdirectedHost,
    /// The `Origin` header names another site (403).
    ForbiddenOrigin,
    /// The bearer token is missing or wrong (401).
    Unauthorized,
    /// The account may not do this: the install's own settings, or an action that needs the
    /// install token (403).
    Forbidden,
    /// The request is malformed: bad JSON, a bad path or query value (400).
    BadRequest,
    /// The body isn't `application/json` (415).
    UnsupportedMediaType,
    /// The body is too large (413).
    PayloadTooLarge,
    /// A value in the request is out of range or refused (422).
    Invalid,
    /// No such route, rule or pending request (404).
    NotFound,
    /// The request conflicts with the current state, e.g. an already decided request (409).
    Conflict,
    /// Two identities on one workspace cover the same owner or both cover the rest of a host
    /// (409); the message names both.
    IdentityCollision,
    /// The stored settings were written by a newer Puddle and can't be changed by this one (409).
    NewerSettings,
    /// A stored settings document is damaged or cannot be read, so the settings cannot be shown
    /// or changed until it is fixed or removed (409); the message says what is wrong.
    SettingsUnreadable,
    /// The settings were changed by someone else since the client read them, so the save was
    /// refused and nothing was written (409); read them again.
    SettingsChanged,
    /// Too many sign-in attempts or error reports: wait for the time in `Retry-After` (429).
    RateLimited,
    /// The feature isn't available in this build or state (503).
    Unavailable,
    /// Puddle failed; the details are in its log (500).
    Internal,
}

/// The input of a request a refusal is about, so a form can put the message beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorField {
    /// The workspace's name.
    Name,
    /// The repository list as a whole (too many entries), or the address of the entry `row`
    /// names.
    RepoUrl,
    /// The image.
    Image,
    /// The memory size.
    MemoryMib,
    /// The vCPU count.
    Cpus,
    /// The start folder.
    StartFolder,
    /// With `row`: that entry's `local_path`.
    LocalPath,
    /// With `row`: that entry's `url`.
    Url,
    /// With `row`: that entry's `directory`.
    Directory,
}

/// The body of every error response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ApiErrorBody {
    /// Stable error code.
    pub error: ErrorCode,
    /// Human-readable, lower case, no trailing period. Never contains a token.
    pub message: String,
    /// The input the refusal is about; `null` when it is about no one input. Set on the
    /// refusals of a workspace's name, repository list, start folder, image and memory.
    #[schema(required = true)]
    pub field: Option<ErrorField>,
    /// The place, counted from 1, of the entry of the request's `repositories` list the refusal
    /// is about; `field` then names the entry's input. `null` when it is about no entry.
    #[schema(required = true, minimum = 1)]
    pub row: Option<u32>,
}

impl std::fmt::Display for ApiErrorBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiErrorBody {}

/// An error a handler or the guard returns; becomes a status and an [`ApiErrorBody`].
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    body: ApiErrorBody,
    /// What went wrong, for the host's own use; never serialized (the client of an internal
    /// error learns only that it failed).
    detail: Option<String>,
    /// Seconds for `Retry-After`, on a refusal that says to wait.
    retry_after: Option<u64>,
}

impl ApiError {
    /// Builds the wire form from its source values.
    pub fn new(status: StatusCode, error: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ApiErrorBody {
                error,
                message: message.into(),
                field: None,
                row: None,
            },
            detail: None,
            retry_after: None,
        }
    }

    /// Says the refusal is about the request's input `field`.
    #[must_use]
    pub fn at_field(mut self, field: ErrorField) -> Self {
        self.body.field = Some(field);
        self
    }

    /// Says the refusal is about the input `field` of the entry `row` (from 1) of the list.
    #[must_use]
    pub fn at_row(mut self, row: usize, field: ErrorField) -> Self {
        self.body.field = Some(field);
        self.body.row = u32::try_from(row).ok();
        self
    }

    /// Too many attempts: try again in `seconds`.
    #[must_use]
    pub fn rate_limited(seconds: u64) -> Self {
        Self::too_many(
            format!("too many sign-in attempts; try again in {seconds} seconds"),
            seconds,
        )
    }

    /// Too many requests of this kind: `message` says which, `Retry-After` is `seconds`.
    pub fn too_many(message: impl Into<String>, seconds: u64) -> Self {
        let mut err = Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::RateLimited,
            message,
        );
        err.retry_after = Some(seconds);
        err
    }

    /// The not found.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, message)
    }

    /// The invalid.
    pub fn invalid(message: impl std::fmt::Display) -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::Invalid,
            message.to_string(),
        )
    }

    /// The bad request.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, message)
    }

    /// A failure inside Puddle. `detail` is logged here (this is where the error is consumed);
    /// the client only learns that something failed.
    pub fn internal(detail: &dyn std::fmt::Display) -> Self {
        tracing::error!(error = %detail, "api request failed");
        let mut err = Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            "internal error; see Puddle's log",
        );
        err.detail = Some(detail.to_string());
        err
    }

    /// A save made from settings that have changed since.
    #[must_use]
    pub fn settings_changed() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            ErrorCode::SettingsChanged,
            "The settings changed in the meantime (another window or the command line). Review the current values and save again.",
        )
    }

    /// Stored settings that cannot be read: the message names `what` and the cause, so the
    /// settings page can show it; `reason` stays the bare cause.
    pub fn settings_unreadable(what: &str, cause: &dyn std::fmt::Display) -> Self {
        tracing::warn!(%what, error = %cause, "settings cannot be read");
        let mut err = Self::new(
            StatusCode::CONFLICT,
            ErrorCode::SettingsUnreadable,
            format!(
                "Puddle cannot read {what}: {cause}; fix that file, or remove it to go back to the defaults"
            ),
        );
        err.detail = Some(cause.to_string());
        err
    }

    /// The cause as the host sees it: the detail of an internal error, else the message.
    #[must_use]
    pub fn reason(&self) -> &str {
        self.detail.as_deref().unwrap_or(&self.body.message)
    }

    /// What the refusal says.
    /// The message a client would read.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.body.message
    }

    /// The body a client would get.
    #[must_use]
    pub fn into_body(self) -> ApiErrorBody {
        self.body
    }

    /// The HTTP status.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The stable error code.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        self.body.error
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (self.status, Json(self.body)).into_response();
        if let Some(seconds) = self.retry_after {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
        }
        if self.status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"puddle\""),
            );
        }
        response
    }
}

impl From<StoreError> for ApiError {
    fn from(err: StoreError) -> Self {
        match err {
            StoreError::UnknownPending(_)
            | StoreError::UnknownSshAgentAsk(_)
            | StoreError::UnknownRule(_)
            | StoreError::UnknownRequestRule(_)
            | StoreError::UnknownRuleSet(_)
            | StoreError::UnknownIdentity(_)
            | StoreError::UnknownRepo(_)
            | StoreError::UnknownEnv { .. }
            // Worded as the workspace service words an unknown id, so another user's workspace
            // answers exactly as one that does not exist.
            | StoreError::UnknownWorkspace(_) => Self::not_found(err.to_string()),
            StoreError::IdentityCollision(_) => Self::new(
                StatusCode::CONFLICT,
                ErrorCode::IdentityCollision,
                err.to_string(),
            ),
            StoreError::PendingNotOpen { .. }
            | StoreError::SshAgentAskNotOpen { .. }
            | StoreError::RuleSetOff { .. }
            | StoreError::RuleSetHidden(_)
            | StoreError::IdentityLabelTaken(_)
            | StoreError::IdentityAttached { .. }
            | StoreError::RepoListed(_)
                        | StoreError::SetupDone
            | StoreError::OwnerKept => {
                Self::new(StatusCode::CONFLICT, ErrorCode::Conflict, err.to_string())
            }
            StoreError::UserName(_)
            | StoreError::DeviceName(_)
            | StoreError::SignInMethod(_)
            | StoreError::Pattern(_)
            | StoreError::ExpiryNotInFuture
            | StoreError::PrecedenceOutOfRange(_)
            | StoreError::NotSwitchable
            | StoreError::RuleSetName(_)
            | StoreError::IdentityInvalid(_)
            | StoreError::EnvInvalid(_)
            | StoreError::ShellInitInvalid(_)
            | StoreError::RequestRuleInvalid(_)
            | StoreError::TooManySshAgentHosts(_)
            | StoreError::TooManySshAgentAsks(_) => Self::invalid(&err),
            _ => Self::internal(&err),
        }
    }
}

impl From<SettingsError> for ApiError {
    fn from(err: SettingsError) -> Self {
        match err {
            SettingsError::NewerSchema { .. } => Self::new(
                StatusCode::CONFLICT,
                ErrorCode::NewerSettings,
                format!("{err}; update Puddle to change these settings"),
            ),
            _ => Self::internal(&err),
        }
    }
}

impl From<WorkspaceError> for ApiError {
    fn from(err: WorkspaceError) -> Self {
        match err {
            WorkspaceError::NotFound(m) => Self::not_found(m),
            WorkspaceError::Conflict(m) => Self::new(StatusCode::CONFLICT, ErrorCode::Conflict, m),
            WorkspaceError::Invalid(m) => Self::invalid(m),
            WorkspaceError::Unavailable(m) => {
                Self::new(StatusCode::SERVICE_UNAVAILABLE, ErrorCode::Unavailable, m)
            }
            WorkspaceError::Internal(m) => Self::internal(&m),
        }
    }
}

impl From<NetworkHealthError> for ApiError {
    fn from(err: NetworkHealthError) -> Self {
        match err {
            NetworkHealthError::Unavailable(m) => {
                Self::new(StatusCode::SERVICE_UNAVAILABLE, ErrorCode::Unavailable, m)
            }
        }
    }
}

impl From<ProxySignInError> for ApiError {
    fn from(err: ProxySignInError) -> Self {
        match err {
            ProxySignInError::BadUser | ProxySignInError::BadPassword => Self::invalid(&err),
            ProxySignInError::Store | ProxySignInError::Unavailable(_) => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::Unavailable,
                err.to_string(),
            ),
        }
    }
}

impl From<CredentialsError> for ApiError {
    fn from(err: CredentialsError) -> Self {
        match err {
            CredentialsError::Unavailable(m) => {
                Self::new(StatusCode::SERVICE_UNAVAILABLE, ErrorCode::Unavailable, m)
            }
            CredentialsError::Invalid(m) => Self::invalid(m),
            CredentialsError::Internal(m) => Self::internal(&m),
        }
    }
}

impl From<ReposError> for ApiError {
    fn from(err: ReposError) -> Self {
        match err {
            ReposError::Unavailable(m) => {
                Self::new(StatusCode::SERVICE_UNAVAILABLE, ErrorCode::Unavailable, m)
            }
        }
    }
}

impl From<BackupServiceError> for ApiError {
    fn from(err: BackupServiceError) -> Self {
        match err {
            // `Failed` names what could not be read or written (a path or a reason, no content).
            BackupServiceError::Unavailable(m) | BackupServiceError::Failed(m) => {
                Self::new(StatusCode::SERVICE_UNAVAILABLE, ErrorCode::Unavailable, m)
            }
        }
    }
}

impl From<DoctorError> for ApiError {
    fn from(err: DoctorError) -> Self {
        match err {
            DoctorError::Unavailable(m) => {
                Self::new(StatusCode::SERVICE_UNAVAILABLE, ErrorCode::Unavailable, m)
            }
            DoctorError::Crashed(_) => Self::internal(&err),
        }
    }
}

impl From<PortsServiceError> for ApiError {
    fn from(err: PortsServiceError) -> Self {
        match err {
            // `Failed` names what could not be read or saved (no secret in it): say it.
            PortsServiceError::Unavailable(m) | PortsServiceError::Failed(m) => {
                Self::new(StatusCode::SERVICE_UNAVAILABLE, ErrorCode::Unavailable, m)
            }
            PortsServiceError::NotRunning(m) | PortsServiceError::Taken(m) => {
                Self::new(StatusCode::CONFLICT, ErrorCode::Conflict, m)
            }
            PortsServiceError::Invalid(m) => Self::invalid(m),
            PortsServiceError::NotForwarded(m) => Self::not_found(m),
        }
    }
}

impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        let message = rejection.body_text();
        match rejection {
            JsonRejection::MissingJsonContentType(_) => Self::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                ErrorCode::UnsupportedMediaType,
                "the body must be application/json",
            ),
            JsonRejection::BytesRejection(ref bytes)
                if bytes.status() == StatusCode::PAYLOAD_TOO_LARGE =>
            {
                Self::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    ErrorCode::PayloadTooLarge,
                    "the body is too large",
                )
            }
            _ => Self::bad_request(message),
        }
    }
}

impl From<PathRejection> for ApiError {
    fn from(rejection: PathRejection) -> Self {
        Self::bad_request(rejection.body_text())
    }
}

impl From<QueryRejection> for ApiError {
    fn from(rejection: QueryRejection) -> Self {
        Self::bad_request(rejection.body_text())
    }
}

impl From<RestoreServiceError> for ApiError {
    fn from(err: RestoreServiceError) -> Self {
        match err {
            RestoreServiceError::Refused(m) => Self::invalid(m),
            RestoreServiceError::NotStaged(m) => Self::not_found(m),
            RestoreServiceError::Unavailable(m) | RestoreServiceError::Failed(m) => {
                Self::new(StatusCode::SERVICE_UNAVAILABLE, ErrorCode::Unavailable, m)
            }
        }
    }
}

impl From<TerminalError> for ApiError {
    fn from(err: TerminalError) -> Self {
        match err {
            TerminalError::Unavailable(m) => {
                Self::new(StatusCode::SERVICE_UNAVAILABLE, ErrorCode::Unavailable, m)
            }
            TerminalError::NotRunning(m) => Self::new(StatusCode::CONFLICT, ErrorCode::Conflict, m),
            TerminalError::Ended(end) => Self::new(
                StatusCode::CONFLICT,
                ErrorCode::Conflict,
                end.message.unwrap_or_else(|| "the terminal ended".into()),
            ),
        }
    }
}

/// Runs blocking work (`SQLite`, settings storage) off the async threads. A panic in it becomes a
/// 500, never a crashed server.
///
/// # Errors
/// An invalid-request or internal [`ApiError`].
pub async fn blocking<T, F>(work: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ApiError> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| ApiError::internal(&err))?
}

#[cfg(test)]
mod tests;
