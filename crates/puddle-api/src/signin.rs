// SPDX-License-Identifier: AGPL-3.0-or-later
//! Browser sign-in: the first visitor makes the owner account, passwords, sessions and devices.
//!
//! # The session
//!
//! A sign-in makes a *device* (one row per browser, listed with a revoke button) and gives the
//! browser two things:
//!
//! - a **refresh token** in an `HttpOnly`, `Secure`, `SameSite=Strict`, `__Host-` cookie, valid
//!   while the device is used (30 days idle expiry, slid on every refresh). Only its SHA-256
//!   digest is stored. It is sent to the two cookie routes (`/api/auth/refresh`, `/api/auth/sign-out`)
//!   alone, and the guard demands an `Origin` of the page's own on every cookie-using `POST`, so a
//!   page of another site cannot use it (no CSRF path: the rest of the API takes no cookie).
//! - a 15-minute **access token**, in the body, kept in the page's memory and sent as a bearer
//!   token like the install token is. It is only valid for a device that is neither revoked nor
//!   expired, checked on every request, so a revoke ends it at once.
//!
//! Every sign-in makes a new device and a new refresh token; nothing a client sends picks or
//! reuses a session, so there is no session fixation. A password change or reset signs every
//! other device out.
//!
//! Revoking a device ends its access tokens, its refresh token and its event streams.
//!
//! The install token (the desktop window, the command line on the server) never signs in and is
//! not accepted on a public address; a device is accepted on public addresses only.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use puddle_store::{
    AccountSecrets, Clock, DeviceId, Role, SignInMethod, Store, StoreError, UserId, UserName,
};
use tokio::sync::broadcast;
use zeroize::Zeroizing;

use crate::authz::{Principal, Via};
use crate::error::{ApiError, ErrorCode, blocking};
use crate::wire::{
    AuthStatus, DeviceList, DeviceView, SessionGrant, SignInMethodView, SignInMethods, UserView,
};

mod limiter;
mod password;
mod tokens;

use limiter::Limiter;
use password::PasswordError;
use tokens::{AccessTokens, digest, random_secret};

/// How long a device works without being used: 30 days.
pub(crate) const DEVICE_TTL_MS: u64 = 30 * 24 * 60 * 60 * 1000;
/// How long a reset invite works: 24 hours.
pub const INVITE_TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// How often an open stream asks whether its device was revoked from another process.
const RECHECK: Duration = Duration::from_secs(15);
/// The name of the refresh cookie.
pub(crate) const COOKIE_NAME: &str = "__Host-puddle-session";
const DEFAULT_DEVICE_NAME: &str = "Browser";

/// What a sign-in produced: the grant for the body and the refresh token for the cookie.
pub(crate) struct Granted {
    pub(crate) grant: SessionGrant,
    pub(crate) refresh: Zeroizing<String>,
}

impl std::fmt::Debug for Granted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Granted")
            .field("grant", &self.grant)
            .finish_non_exhaustive()
    }
}

/// Browser sign-in for one running API.
pub(crate) struct SignIn {
    store: Arc<Store>,
    secrets: Arc<AccountSecrets>,
    clock: Arc<dyn Clock>,
    tokens: AccessTokens,
    limiter: Mutex<Limiter>,
    /// A hash to check an unknown name against, so it takes as long as a wrong password.
    decoy: Zeroizing<String>,
    /// Whether users other than the owner may sign in.
    multi_user: bool,
    revoked: broadcast::Sender<DeviceId>,
}

impl std::fmt::Debug for SignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignIn").finish_non_exhaustive()
    }
}

fn internal(err: &dyn std::fmt::Display) -> ApiError {
    ApiError::internal(err)
}

fn wrong_credentials() -> ApiError {
    ApiError::new(
        axum::http::StatusCode::UNAUTHORIZED,
        ErrorCode::Unauthorized,
        "wrong name or password",
    )
}

fn weak(err: PasswordError) -> ApiError {
    match err {
        PasswordError::Weak(why) => ApiError::invalid(why),
        PasswordError::Failed(why) => internal(&why),
    }
}

impl SignIn {
    /// Sign-in over `store`'s accounts.
    ///
    /// # Errors
    /// [`PasswordError`] when the system has no randomness to make the decoy hash.
    pub(crate) fn new(
        store: Arc<Store>,
        secrets: Arc<AccountSecrets>,
        clock: Arc<dyn Clock>,
        multi_user: bool,
    ) -> Result<Self, ApiError> {
        let (revoked, _) = broadcast::channel(64);
        Ok(Self {
            store,
            secrets,
            clock,
            tokens: AccessTokens::default(),
            limiter: Mutex::new(Limiter::default()),
            decoy: password::decoy().map_err(weak)?,
            multi_user,
            revoked,
        })
    }

    fn now(&self) -> u64 {
        self.clock.now_ms()
    }

    fn admit(&self, key: Option<&str>) -> Result<(), ApiError> {
        let now = self.now();
        let mut limiter = self
            .limiter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match key {
            Some(key) => limiter.admit(key, now),
            None => limiter.admit_any(now),
        }
        .map_err(|later| ApiError::rate_limited(later.seconds()))
    }

    fn record(&self, key: &str, right: bool) {
        let now = self.now();
        let mut limiter = self
            .limiter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if right {
            limiter.succeeded(key);
        } else {
            limiter.failed(key, now);
        }
    }

    async fn password_login_on(self: &Arc<Self>) -> Result<bool, ApiError> {
        let this = self.clone();
        let methods = blocking(move || Ok(this.store.sign_in_methods()?)).await?;
        Ok(methods
            .iter()
            .any(|&(method, on)| method == SignInMethod::Password && on))
    }

    async fn require_password_login(self: &Arc<Self>) -> Result<(), ApiError> {
        if self.password_login_on().await? {
            Ok(())
        } else {
            Err(ApiError::new(
                axum::http::StatusCode::FORBIDDEN,
                ErrorCode::Forbidden,
                "signing in with a password is switched off",
            ))
        }
    }

    /// Whether the first visitor still has to make the owner account, and whether passwords sign
    /// in.
    pub(crate) async fn status(self: &Arc<Self>) -> Result<AuthStatus, ApiError> {
        let this = self.clone();
        let (setup_done, methods) =
            blocking(move || Ok((this.secrets.setup_done()?, this.store.sign_in_methods()?)))
                .await?;
        Ok(AuthStatus {
            setup_open: !setup_done,
            password_login: methods
                .iter()
                .any(|&(method, on)| method == SignInMethod::Password && on),
        })
    }

    /// The first visitor makes the owner account and is signed in as it.
    pub(crate) async fn setup(
        self: &Arc<Self>,
        name: &str,
        password: &str,
        device_name: Option<&str>,
    ) -> Result<Granted, ApiError> {
        let name = UserName::new(name)?;
        password::check_policy(password, name.as_str()).map_err(weak)?;
        self.require_password_login().await?;
        self.admit(None)?;
        let this = self.clone();
        let password = Zeroizing::new(password.to_owned());
        let device_name = device_name.map(str::to_owned);
        blocking(move || {
            if this.secrets.setup_done()? {
                return Err(StoreError::SetupDone.into());
            }
            let hash = password::hash(&password).map_err(weak)?;
            this.secrets.create_owner(&name, &hash)?;
            tracing::info!("the owner account was made");
            this.make_session(UserId::OWNER, device_name.as_deref())
        })
        .await
    }

    /// A password sign-in.
    pub(crate) async fn sign_in(
        self: &Arc<Self>,
        name: &str,
        password: &str,
        device_name: Option<&str>,
    ) -> Result<Granted, ApiError> {
        self.require_password_login().await?;
        let key = name.to_ascii_lowercase();
        self.admit(Some(&key))?;
        let this = self.clone();
        let name = name.to_owned();
        let password = Zeroizing::new(password.to_owned());
        let device_name = device_name.map(str::to_owned);
        let user = blocking(move || {
            // A name that is not an account, or an account that may not sign in (users other
            // than the owner while several users are off), is checked against the decoy: the same
            // work, the same answer.
            let user = this
                .store
                .user_named(&name)?
                .filter(|u| this.multi_user || u.role == Role::Owner);
            let right = if let Some(user) = &user {
                let right = this
                    .secrets
                    .check_password(user.id, |hash| password::verify(hash, &password))?;
                if !right && !this.secrets.has_password(user.id)? {
                    password::verify(&this.decoy, &password);
                }
                right
            } else {
                password::verify(&this.decoy, &password);
                false
            };
            Ok(user.filter(|_| right).map(|u| u.id))
        })
        .await?;
        self.record(&key, user.is_some());
        let user = user.ok_or_else(wrong_credentials)?;
        let this = self.clone();
        blocking(move || this.make_session(user, device_name.as_deref())).await
    }

    /// Sets a password with the secret of an invite link (made on the server) and signs in.
    pub(crate) async fn redeem_invite(
        self: &Arc<Self>,
        secret: &str,
        password: &str,
        device_name: Option<&str>,
    ) -> Result<Granted, ApiError> {
        self.require_password_login().await?;
        // The name is not known before the invite is used up, so the policy is the name-free part.
        password::check_policy(password, "").map_err(weak)?;
        self.admit(None)?;
        let this = self.clone();
        let digest = digest(secret);
        let password = Zeroizing::new(password.to_owned());
        let device_name = device_name.map(str::to_owned);
        let (granted, ended) = blocking(move || {
            let user = this.secrets.redeem_invite(&digest)?.ok_or_else(|| {
                ApiError::new(
                    axum::http::StatusCode::UNAUTHORIZED,
                    ErrorCode::Unauthorized,
                    "this invite link is not valid any more",
                )
            })?;
            let hash = password::hash(&password).map_err(weak)?;
            this.secrets.set_password_hash(user, &hash)?;
            let ended = this.store.revoke_devices_of(user, None)?;
            Ok((this.make_session(user, device_name.as_deref())?, ended))
        })
        .await?;
        self.end_devices(&ended);
        Ok(granted)
    }

    /// A new device for `user` with its refresh token and first access token.
    fn make_session(&self, user: UserId, device_name: Option<&str>) -> Result<Granted, ApiError> {
        let now = self.now();
        let device = self.store.add_device(
            user,
            device_name.unwrap_or(DEFAULT_DEVICE_NAME),
            now.saturating_add(DEVICE_TTL_MS),
        )?;
        let refresh = random_secret().map_err(|e| internal(&e))?;
        self.secrets.put_session(device.id, &digest(&refresh))?;
        let grant = self.grant(user, &device)?;
        Ok(Granted { grant, refresh })
    }

    fn grant(&self, user: UserId, device: &puddle_store::Device) -> Result<SessionGrant, ApiError> {
        let (access, expires_at) = self
            .tokens
            .issue(device.id, self.now())
            .map_err(|e| internal(&e))?;
        let user = self.store.user(user)?;
        Ok(SessionGrant {
            access_token: access.to_string(),
            expires_at,
            user: UserView {
                id: user.id.get(),
                name: user.name,
                role: user.role.as_str().to_owned(),
            },
            device: DeviceView::of(device, true),
        })
    }

    /// A new access token for the device the refresh token `refresh` belongs to; the device's
    /// 30 days start again.
    pub(crate) async fn refresh(self: &Arc<Self>, refresh: &str) -> Result<SessionGrant, ApiError> {
        let this = self.clone();
        let digest = digest(refresh);
        blocking(move || {
            let session = this.secrets.session_for(&digest)?.ok_or_else(|| {
                ApiError::new(
                    axum::http::StatusCode::UNAUTHORIZED,
                    ErrorCode::Unauthorized,
                    "this browser is not signed in",
                )
            })?;
            this.store
                .touch_device(session.device, this.now().saturating_add(DEVICE_TTL_MS))?;
            let device = this
                .store
                .devices(session.user)?
                .into_iter()
                .find(|d| d.id == session.device)
                .ok_or_else(|| internal(&"a device vanished while it refreshed"))?;
            this.grant(session.user, &device)
        })
        .await
    }

    /// Signs out the device `refresh` belongs to, if it is a live one.
    pub(crate) async fn sign_out(self: &Arc<Self>, refresh: &str) -> Result<(), ApiError> {
        let this = self.clone();
        let digest = digest(refresh);
        let ended = blocking(move || {
            let Some(session) = this.secrets.session_for(&digest)? else {
                return Ok(None);
            };
            this.store.revoke_device(session.user, session.device)?;
            Ok(Some(session.device))
        })
        .await?;
        if let Some(device) = ended {
            self.end_devices(&[device]);
        }
        Ok(())
    }

    /// Changes `user`'s password, and signs out every device but `device`.
    pub(crate) async fn change_password(
        self: &Arc<Self>,
        user: UserId,
        device: DeviceId,
        current: &str,
        new: &str,
    ) -> Result<(), ApiError> {
        let this = self.clone();
        let who = blocking({
            let this = this.clone();
            move || Ok(this.store.user(user)?)
        })
        .await?;
        password::check_policy(new, &who.name).map_err(weak)?;
        self.require_password_login().await?;
        let key = who.name.to_ascii_lowercase();
        self.admit(Some(&key))?;
        let current = Zeroizing::new(current.to_owned());
        let new = Zeroizing::new(new.to_owned());
        let verified = blocking({
            let this = this.clone();
            move || {
                Ok(this
                    .secrets
                    .check_password(user, |hash| password::verify(hash, &current))?)
            }
        })
        .await?;
        self.record(&key, verified);
        if !verified {
            return Err(wrong_credentials());
        }
        let ended = blocking(move || {
            let hash = password::hash(&new).map_err(weak)?;
            this.secrets.set_password_hash(user, &hash)?;
            Ok(this.store.revoke_devices_of(user, Some(device))?)
        })
        .await?;
        self.end_devices(&ended);
        Ok(())
    }

    /// `user`'s devices; `current` is marked.
    pub(crate) async fn devices(
        self: &Arc<Self>,
        user: UserId,
        current: Option<DeviceId>,
    ) -> Result<DeviceList, ApiError> {
        let this = self.clone();
        let devices = blocking(move || Ok(this.store.devices(user)?)).await?;
        Ok(DeviceList {
            devices: devices
                .iter()
                .map(|d| DeviceView::of(d, Some(d.id) == current))
                .collect(),
        })
    }

    /// Revokes `user`'s `device`. Someone else's device answers as one that does not exist.
    pub(crate) async fn revoke_device(
        self: &Arc<Self>,
        user: UserId,
        device: DeviceId,
    ) -> Result<(), ApiError> {
        let this = self.clone();
        let revoked = blocking(move || Ok(this.store.revoke_device(user, device)?)).await?;
        if !revoked {
            return Err(ApiError::not_found(format!("no device {}", device.get())));
        }
        self.end_devices(&[device]);
        Ok(())
    }

    /// The sign-in methods that exist.
    pub(crate) async fn methods(self: &Arc<Self>) -> Result<SignInMethods, ApiError> {
        let this = self.clone();
        let methods = blocking(move || Ok(this.store.sign_in_methods()?)).await?;
        Ok(SignInMethods {
            methods: methods
                .into_iter()
                .map(|(method, enabled)| SignInMethodView {
                    method: method.into(),
                    enabled,
                })
                .collect(),
        })
    }

    /// Switches a method on or off; the last one that is on stays on.
    pub(crate) async fn set_method(
        self: &Arc<Self>,
        method: SignInMethod,
        enabled: bool,
    ) -> Result<SignInMethods, ApiError> {
        let this = self.clone();
        blocking(move || Ok(this.store.set_sign_in_method(method, enabled)?)).await?;
        self.methods().await
    }

    /// Who the access token `token` is, while it has not run out and its device is neither
    /// revoked nor expired. Revoking from another process (the server's command line) counts at
    /// once, because the database is asked every time.
    pub(crate) async fn principal_for(self: &Arc<Self>, token: &str) -> Option<Principal> {
        let device = self.tokens.device_of(token, self.now())?;
        let this = self.clone();
        let session = blocking(move || Ok(this.store.device_session(device)?))
            .await
            .ok()??;
        Some(Principal {
            user: session.user,
            role: session.role,
            via: Via::Device(session.device),
        })
    }

    fn end_devices(&self, devices: &[DeviceId]) {
        for &device in devices {
            self.tokens.revoke(device);
            // Nobody listening is fine: no stream is open.
            let _ = self.revoked.send(device);
        }
    }

    /// Completes when `device` is revoked or expires: what an event stream waits for to end.
    pub(crate) async fn ended(self: Arc<Self>, device: DeviceId) {
        // Subscribe before the first look, so a revoke in between is not missed.
        let mut revoked = self.revoked.subscribe();
        loop {
            if !self.still_works(device).await {
                return;
            }
            tokio::select! {
                got = revoked.recv() => match got {
                    Ok(id) if id == device => return,
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                () = tokio::time::sleep(RECHECK) => {}
            }
        }
    }

    async fn still_works(self: &Arc<Self>, device: DeviceId) -> bool {
        let this = self.clone();
        // A database that cannot answer ends the stream: it must not outlive a revoke it cannot see.
        blocking(move || Ok(this.store.device_session(device)?))
            .await
            .ok()
            .flatten()
            .is_some()
    }
}

/// What a server-side reset hands the operator: the secret to put after `#` in the invite link.
#[derive(Debug)]
pub struct ResetInvite {
    /// The link's secret (43 characters).
    pub secret: Zeroizing<String>,
    /// When the link stops working, epoch ms.
    pub expires_at: u64,
}

/// Why a reset failed.
#[derive(Debug, thiserror::Error)]
pub enum ResetError {
    /// No user has this name.
    #[error("no user named {0}")]
    UnknownUser(String),
    /// The database refused.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The system has no randomness.
    #[error("cannot make a secret: {0}")]
    Random(String),
}

/// The server-side password reset: takes `name`'s password away, signs every one of its devices
/// out, and makes a one-time invite whose secret the operator puts into a link. Nothing here sets
/// or prints a password. The first-visitor flow is never reopened by it.
///
/// # Errors
/// [`ResetError`].
pub fn reset_account(
    store: &Store,
    secrets: &AccountSecrets,
    name: &str,
    now: u64,
) -> Result<ResetInvite, ResetError> {
    let user = store
        .user_named(name)?
        .ok_or_else(|| ResetError::UnknownUser(name.to_owned()))?;
    let secret = random_secret().map_err(|e| ResetError::Random(e.to_string()))?;
    let expires_at = now.saturating_add(INVITE_TTL_MS);
    secrets.clear_password(user.id)?;
    store.revoke_devices_of(user.id, None)?;
    secrets.put_invite(user.id, &digest(&secret), expires_at)?;
    Ok(ResetInvite { secret, expires_at })
}

#[cfg(test)]
pub(crate) mod tests;
