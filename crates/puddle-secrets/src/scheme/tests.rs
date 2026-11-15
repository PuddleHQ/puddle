// SPDX-License-Identifier: AGPL-3.0-or-later
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
