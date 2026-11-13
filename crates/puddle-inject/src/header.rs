// SPDX-License-Identifier: GPL-3.0-or-later
//! The `Authorization` value a Git host takes for a token.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use puddle_proxy::SecretValue;
use puddle_secrets::{TokenScheme, azure_devops_scheme};
use zeroize::Zeroizing;

/// The user name GitHub documents for token authentication over HTTPS; the host looks at the
/// token only.
const TOKEN_USER: &str = "x-access-token";

/// What `token` goes out as to the Git host `credential_host`.
///
/// Azure DevOps takes a personal access token as the password of an empty user name, and an Entra
/// access token (a JWT, what Git Credential Manager signs in with) as a bearer token. Every other
/// host gets HTTP Basic with `x-access-token` as the user name, which GitHub, GitLab and Gitea
/// accept for personal, OAuth and fine-grained tokens.
pub(crate) fn authorization(credential_host: &str, token: &str) -> SecretValue {
    if credential_host == "dev.azure.com" {
        return match azure_devops_scheme(token) {
            TokenScheme::Bearer => SecretValue::new(prefixed("Bearer ", token)),
            TokenScheme::BasicPassword => basic("", token),
        };
    }
    basic(TOKEN_USER, token)
}

/// `Basic base64(user:token)`. The `user:token` temporary and the pieces of the value are sized
/// first and overwritten when they drop, so no copy of the token is freed as it is.
fn basic(user: &str, token: &str) -> SecretValue {
    const PREFIX: &str = "Basic ";
    let mut plain = Zeroizing::new(String::with_capacity(user.len() + 1 + token.len()));
    plain.push_str(user);
    plain.push(':');
    plain.push_str(token);
    let encoded = base64::encoded_len(plain.len(), true).unwrap_or_default();
    let mut value = String::with_capacity(PREFIX.len() + encoded);
    value.push_str(PREFIX);
    STANDARD.encode_string(plain.as_bytes(), &mut value);
    SecretValue::new(value)
}

fn prefixed(prefix: &str, token: &str) -> String {
    let mut value = String::with_capacity(prefix.len() + token.len());
    value.push_str(prefix);
    value.push_str(token);
    value
}

#[cfg(test)]
mod tests;
