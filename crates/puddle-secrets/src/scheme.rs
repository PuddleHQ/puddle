// SPDX-License-Identifier: GPL-3.0-or-later
//! How a token for Azure DevOps goes into an `Authorization` header: the one place that decides
//! it, for the Git injector and for repository discovery alike.

/// The `Authorization` scheme a token goes out under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
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
mod tests {
    use super::*;

    #[test]
    fn an_entra_token_is_a_bearer_and_a_personal_access_token_is_basic() {
        let jwt = "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJ4In0.c2lnbmF0dXJl";
        assert_eq!(azure_devops_scheme(jwt), TokenScheme::Bearer);
        // Padding and the url-safe characters belong to base64url.
        assert_eq!(
            azure_devops_scheme("eyJhb-_=.eyJ_-.c2ln=="),
            TokenScheme::Bearer
        );
        // A personal access token is 52 characters of letters and digits.
        for pat in ["", "abc123", "plainpatwithnodots", "eyJ-but-no-dots"] {
            assert_eq!(
                azure_devops_scheme(pat),
                TokenScheme::BasicPassword,
                "{pat}"
            );
        }
    }

    #[test]
    fn things_that_only_look_like_an_entra_token_stay_basic() {
        for token in [
            "eyJhbGciOiJSUzI1NiJ9.onlytwo",
            "eyJhbGciOi.a.b.c",
            "notjwt.eyJzdWIiOiJ4In0.c2lnbmF0dXJl",
            "eyJhbGciOi.has space.c2ln",
            "eyJhbGciOi..c2ln",
            "eyJa.b\u{7f}.c",
            "eyJa.b\n.c",
        ] {
            assert_eq!(
                azure_devops_scheme(token),
                TokenScheme::BasicPassword,
                "{token:?}"
            );
        }
    }
}
