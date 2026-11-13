// SPDX-License-Identifier: AGPL-3.0-or-later
//! Browser sign-in on the wire. Only [`SessionGrant`] carries a credential (the access token a
//! sign-in hands to the browser that just proved who it is); every other type here is safe to
//! show to anyone who may see it, and a test keeps it that way: no password, hash, refresh token,
//! invite secret or device key is a field of any other response.

use puddle_store as store;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::SecretText;

/// Whether this Puddle can be signed in to, and how. Anyone who can reach the address may read
/// it; it holds no name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuthStatus {
    /// `true` until the first visitor has made the owner account.
    pub setup_open: bool,
    /// Whether a password signs in.
    pub password_login: bool,
}

/// The first visitor makes the owner account.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetupRequest {
    /// The owner's name.
    pub name: String,
    /// The password: at least 12 characters.
    #[schema(value_type = String, format = Password)]
    pub password: SecretText,
    /// What this browser calls itself in the device list; `null` for the default.
    #[serde(default)]
    pub device_name: Option<String>,
}

/// A password sign-in.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PasswordSignInRequest {
    /// The account's name.
    pub name: String,
    /// The password.
    #[schema(value_type = String, format = Password)]
    pub password: SecretText,
    /// What this browser calls itself in the device list; `null` for the default.
    #[serde(default)]
    pub device_name: Option<String>,
}

/// Uses an invite link (made by the server's command line) to set a password.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct InviteRequest {
    /// The secret after `#` in the invite link.
    #[schema(value_type = String, format = Password)]
    pub secret: SecretText,
    /// The new password: at least 12 characters.
    #[schema(value_type = String, format = Password)]
    pub password: SecretText,
    /// What this browser calls itself in the device list; `null` for the default.
    #[serde(default)]
    pub device_name: Option<String>,
}

/// Changes the signed-in user's password. The other devices are signed out.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangePasswordRequest {
    /// The password now.
    #[schema(value_type = String, format = Password)]
    pub current: SecretText,
    /// The new password: at least 12 characters.
    #[schema(value_type = String, format = Password)]
    pub new: SecretText,
}

/// Whose session it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UserView {
    /// The user's id.
    pub id: i64,
    /// The name.
    pub name: String,
    /// `owner` or `user`.
    pub role: String,
}

/// What a sign-in or a refresh gives the browser: an access token to send as
/// `Authorization: Bearer` for the next 15 minutes. The long-lived part, the refresh token, is
/// never in a body: it is in an `HttpOnly` cookie the page cannot read.
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct SessionGrant {
    /// The access token. Keep it in memory only.
    pub access_token: String,
    /// When it stops working, epoch ms; ask for a new one before.
    pub expires_at: u64,
    /// Who signed in.
    pub user: UserView,
    /// This browser's entry in the device list.
    pub device: DeviceView,
}

impl std::fmt::Debug for SessionGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionGrant")
            .field("user", &self.user)
            .field("device", &self.device)
            .finish_non_exhaustive()
    }
}

/// One signed-in browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DeviceView {
    /// The id.
    pub id: i64,
    /// What it calls itself.
    pub name: String,
    /// When it signed in, epoch ms.
    pub created_at: u64,
    /// When it last asked for an access token, epoch ms.
    pub last_seen_at: u64,
    /// When it signs out by itself unless used, epoch ms.
    pub expires_at: u64,
    /// Whether this is the browser asking.
    pub current: bool,
}

impl DeviceView {
    pub(crate) fn of(device: &store::Device, current: bool) -> Self {
        Self {
            id: device.id.get(),
            name: device.name.clone(),
            created_at: device.created_at,
            last_seen_at: device.last_seen_at,
            expires_at: device.expires_at,
            current,
        }
    }
}

/// The signed-in user's devices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DeviceList {
    /// The ones that still work, the one used last first.
    pub devices: Vec<DeviceView>,
}

/// A way of signing in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SignInMethodKind {
    /// A password.
    Password,
    /// A passkey.
    Passkey,
}

impl From<store::SignInMethod> for SignInMethodKind {
    fn from(method: store::SignInMethod) -> Self {
        match method {
            store::SignInMethod::Password => Self::Password,
            store::SignInMethod::Passkey => Self::Passkey,
        }
    }
}

impl From<SignInMethodKind> for store::SignInMethod {
    fn from(kind: SignInMethodKind) -> Self {
        match kind {
            SignInMethodKind::Password => Self::Password,
            SignInMethodKind::Passkey => Self::Passkey,
        }
    }
}

/// One sign-in method and whether it is on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SignInMethodView {
    /// Which.
    pub method: SignInMethodKind,
    /// Whether it signs anyone in.
    pub enabled: bool,
}

/// The sign-in methods that exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SignInMethods {
    /// Each method.
    pub methods: Vec<SignInMethodView>,
}

/// Switches one method on or off. The last method that is on can't be switched off.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SignInMethodRequest {
    /// Which.
    pub method: SignInMethodKind,
    /// The new state.
    pub enabled: bool,
}
