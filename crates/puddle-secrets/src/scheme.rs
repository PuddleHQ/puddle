// SPDX-License-Identifier: AGPL-3.0-or-later
//! How a token for Azure DevOps goes into an `Authorization` header: the one place that decides
//! it, for the Git injector and for repository discovery alike.

/// The `Authorization` scheme a token goes out under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenScheme {
    /// `Authorization: Bearer <token>`.
    Bearer,
    /// `Authorization: Basic base64(:<token>)`, the token as the password of an empty user name.
    BasicPassword,
}

/// The scheme Azure DevOps takes `token` under.
///
/// A personal access token goes as Basic, an Entra access token (a JWT, what Git Credential
/// Manager signs in with) as Bearer. Only a token that is plainly a JWT goes as Bearer: three
/// non-empty base64url parts, the first starting `eyJ` (the base64url of `{"`). Anything else is
/// taken for a personal access token, and a host that does not know it answers with a refusal.
#[must_use]
pub fn azure_devops_scheme(token: &str) -> TokenScheme {
    if looks_like_jwt(token) {
        TokenScheme::Bearer
    } else {
        TokenScheme::BasicPassword
    }
}

fn looks_like_jwt(token: &str) -> bool {
    let parts: Vec<&str> = token.split('.').collect();
    parts.len() == 3
        && parts.first().is_some_and(|head| head.starts_with("eyJ"))
        && parts.iter().all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '='))
        })
}

#[cfg(test)]
mod tests;
