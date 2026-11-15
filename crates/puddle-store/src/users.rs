// SPDX-License-Identifier: AGPL-3.0-or-later
//! The people who use one Puddle: an owner (the operator) and, with several users switched on,
//! plain users who each see only their own workspaces and what belongs to them. No type here
//! carries a credential; those live behind [`crate::AccountSecrets`].

use std::fmt;

/// A user's id: 1 is the owner, made with the database; others are never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UserId(i64);

impl UserId {
    /// The owner: the operator, who holds the install token, and who owns everything made before
    /// there were users and every workspace with no other user recorded.
    pub const OWNER: Self = Self(1);

    /// The id as stored.
    #[must_use]
    pub fn get(self) -> i64 {
        self.0
    }

    /// A user id read back from where it was handed out (a session, a stored entry's name).
    #[must_use]
    pub fn from_stored(id: i64) -> Self {
        Self(id)
    }
}

impl fmt::Display for UserId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// What a user is. The owner manages the install (global settings, users); a user has their own
/// workspaces and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// The operator; exactly one.
    Owner,
    /// Anyone else.
    User,
}

impl Role {
    /// The stored spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::User => "user",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "owner" => Some(Self::Owner),
            "user" => Some(Self::User),
            _ => None,
        }
    }
}

/// The longest user name, in characters.
pub const MAX_USER_NAME_CHARS: usize = 64;

/// A user name: 1 to [`MAX_USER_NAME_CHARS`] characters, no control characters, no leading or
/// trailing space. Unique per install, ignoring ASCII case.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UserName(String);

impl UserName {
    /// Checks `text`.
    ///
    /// # Errors
    /// [`crate::StoreError::UserName`] saying what is wrong.
    pub fn new(text: &str) -> Result<Self, crate::StoreError> {
        let refuse = |why: &str| Err(crate::StoreError::UserName(why.to_owned()));
        let chars = text.chars().count();
        if chars == 0 {
            return refuse("a user name can't be empty");
        }
        if chars > MAX_USER_NAME_CHARS {
            return refuse("a user name has at most 64 characters");
        }
        if text.chars().any(char::is_control) {
            return refuse("a user name can't contain control characters");
        }
        if text.trim() != text {
            return refuse("a user name can't start or end with a space");
        }
        Ok(Self(text.to_owned()))
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One user, as anyone allowed to see the user list sees it: never a credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    /// The id.
    pub id: UserId,
    /// Owner or user.
    pub role: Role,
    /// The name.
    pub name: String,
    /// When the account was made, epoch ms.
    pub created_at: u64,
}

/// A device's id: one signed-in browser of one user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(i64);

impl DeviceId {
    /// The id as stored.
    #[must_use]
    pub fn get(self) -> i64 {
        self.0
    }

    /// A device id read back from where it was handed out (a session).
    #[must_use]
    pub fn from_stored(id: i64) -> Self {
        Self(id)
    }
}

/// The longest device name, in characters.
pub const MAX_DEVICE_NAME_CHARS: usize = 64;

/// One signed-in browser, as its user sees it in the list: never a credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// The id.
    pub id: DeviceId,
    /// What the browser calls itself, as the user can recognise it.
    pub name: String,
    /// When it signed in, epoch ms.
    pub created_at: u64,
    /// When it last asked for an access token, epoch ms.
    pub last_seen_at: u64,
    /// When it stops working unless it is used again, epoch ms.
    pub expires_at: u64,
}

/// Who a device signs in as, while it is neither revoked nor expired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceSession {
    /// The device.
    pub device: DeviceId,
    /// Its user.
    pub user: UserId,
    /// That user's role.
    pub role: Role,
}

/// A way of signing in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignInMethod {
    /// A password.
    Password,
    /// A passkey (not built yet; the row exists only once passkeys do).
    Passkey,
}

impl SignInMethod {
    /// The stored spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Passkey => "passkey",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "password" => Some(Self::Password),
            "passkey" => Some(Self::Passkey),
            _ => None,
        }
    }
}

/// A device name: 1 to [`MAX_DEVICE_NAME_CHARS`] characters, no control characters; surrounding
/// space is trimmed.
///
/// # Errors
/// [`crate::StoreError::DeviceName`] saying what is wrong.
pub fn device_name(text: &str) -> Result<String, crate::StoreError> {
    let text = text.trim();
    let refuse = |why: &str| Err(crate::StoreError::DeviceName(why.to_owned()));
    if text.is_empty() {
        return refuse("a device name can't be empty");
    }
    if text.chars().count() > MAX_DEVICE_NAME_CHARS {
        return refuse("a device name has at most 64 characters");
    }
    if text.chars().any(char::is_control) {
        return refuse("a device name can't contain control characters");
    }
    Ok(text.to_owned())
}

#[cfg(test)]
mod tests;
