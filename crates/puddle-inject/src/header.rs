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
            _ => basic("", token),
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
mod tests {
    use puddle_proxy::InjectedHeader;

    use super::*;

    fn text(value: &SecretValue, expected: &str) -> bool {
        InjectedHeader::new("authorization", value.clone())
            .unwrap()
            .value_is(expected)
    }

    #[test]
    fn github_and_other_hosts_get_basic_with_the_token_user() {
        let value = authorization("github.com", "ghp_abc");
        let want = format!("Basic {}", STANDARD.encode("x-access-token:ghp_abc"));
        assert!(text(&value, &want));
        assert!(text(
            &authorization("git.example", "t"),
            &format!("Basic {}", STANDARD.encode("x-access-token:t"))
        ));
    }

    #[test]
    fn azure_devops_gets_a_pat_as_the_password_and_an_entra_token_as_a_bearer() {
        let pat = authorization("dev.azure.com", "abc123");
        assert!(text(&pat, &format!("Basic {}", STANDARD.encode(":abc123"))));
        let jwt = "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiIxIn0.c2ln";
        assert!(text(
            &authorization("dev.azure.com", jwt),
            &format!("Bearer {jwt}")
        ));
        // Two dots are not enough, nor is a JWT-looking token on another host.
        assert!(text(
            &authorization("dev.azure.com", "eyJabc.def"),
            &format!("Basic {}", STANDARD.encode(":eyJabc.def"))
        ));
        assert!(text(
            &authorization("github.com", jwt),
            &format!("Basic {}", STANDARD.encode(format!("x-access-token:{jwt}")))
        ));
    }

    /// Every kind of token an Azure DevOps credential holds, and the scheme it goes under;
    /// repository discovery's tests walk the same table against the same function.
    const KINDS: [(&str, bool); 7] = [
        ("plainpatwithnodots", false),
        ("eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJ4In0.c2lnbmF0dXJl", true),
        ("eyJhbGciOiJSUzI1NiJ9.onlytwo", false),
        ("notjwt.eyJzdWIiOiJ4In0.c2lnbmF0dXJl", false),
        ("eyJhbGciOi.has space.c2ln", false),
        ("eyJhbGciOi..c2ln", false),
        ("eyJa.b\u{7f}.c", false),
    ];

    #[test]
    fn the_injector_picks_the_scheme_puddle_secrets_picks_for_every_kind_of_token() {
        for (token, bearer) in KINDS {
            assert_eq!(
                azure_devops_scheme(token) == TokenScheme::Bearer,
                bearer,
                "{token:?}"
            );
            let want = if bearer {
                format!("Bearer {token}")
            } else {
                format!("Basic {}", STANDARD.encode(format!(":{token}")))
            };
            assert!(
                text(&authorization("dev.azure.com", token), &want),
                "{token:?}"
            );
        }
    }
}
