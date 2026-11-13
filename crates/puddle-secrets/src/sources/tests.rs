// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

fn target() -> (HostName, UrlPath) {
    (
        HostName::new("dev.azure.com").unwrap(),
        UrlPath::new("org").unwrap(),
    )
}

#[test]
fn fill_answer_must_match_the_request() {
    let (h, p) = target();
    let ok = "protocol=https\nhost=dev.azure.com\npath=org\nusername=me\npassword=CANARY-1\n";
    let got = parse_fill(ok, &h, &p, None).unwrap();
    assert_eq!(got.credential.secret.expose(), "CANARY-1");
    assert_eq!(got.credential.username.as_deref(), Some("me"));
    for bad in [
        "protocol=https\nhost=github.com\npath=org\npassword=x\n",
        "protocol=https\nhost=dev.azure.com\npath=other\npassword=x\n",
        "protocol=https\nhost=dev.azure.com\npassword=x\n",
        "protocol=http\nhost=dev.azure.com\npath=org\npassword=x\n",
        "password=x\n",
    ] {
        assert_eq!(
            parse_fill(bad, &h, &p, None).unwrap_err(),
            SourceError::Refused(Refusal::WrongTarget),
            "{bad:?}"
        );
    }
}

#[test]
fn fill_answer_checks_the_account_and_the_password() {
    let (h, p) = target();
    let me = AccountName::new("me").unwrap();
    let other = "host=dev.azure.com\npath=org\nusername=you\npassword=x\n";
    assert!(parse_fill(other, &h, &p, Some(&me)).is_err());
    let none = "host=dev.azure.com\npath=org/\nusername=me\npassword=\n";
    assert_eq!(
        parse_fill(none, &h, &p, Some(&me)).unwrap_err(),
        SourceError::NotSignedIn
    );
    let spaced = "host=DEV.azure.com\npath=org\npassword=a b\n";
    assert!(parse_fill(spaced, &h, &p, None).is_ok());
    let ctrl = "host=dev.azure.com\npath=org\npassword=a\u{7}b\n";
    assert_eq!(
        parse_fill(ctrl, &h, &p, None).unwrap_err(),
        SourceError::Refused(Refusal::Malformed)
    );
}

#[test]
fn fill_answer_expiry() {
    let (h, p) = target();
    // The remaining time is checked on the pure function, with a fixed clock: it has no secret in
    // reach and no dependence on the machine's speed.
    assert_eq!(remaining_validity(None, 1000), Ok(None));
    assert_eq!(
        remaining_validity(Some(4600), 1000),
        Ok(Some(Duration::from_secs(3600)))
    );
    assert_eq!(
        remaining_validity(Some(1001), 1000),
        Ok(Some(Duration::from_secs(1)))
    );
    assert_eq!(
        remaining_validity(Some(1000), 1000),
        Err(SourceError::NotSignedIn)
    );
    let live = "host=dev.azure.com\npath=org\npassword=x\npassword_expiry_utc=99999999999\n";
    assert!(parse_fill(live, &h, &p, None).unwrap().valid_for.is_some());
    let dead = "host=dev.azure.com\npath=org\npassword=x\npassword_expiry_utc=5\n";
    assert_eq!(
        parse_fill(dead, &h, &p, None).unwrap_err(),
        SourceError::NotSignedIn
    );
}

const GUID: &str = "72f988bf-86f1-41af-91ab-2d7cd011db47";

fn tenant(s: &str) -> TenantId {
    TenantId::new(s).unwrap()
}

#[test]
fn az_answer_is_read_field_by_field() {
    let json = format!(
        r#"{{"accessToken":"CANARY-az","expiresOn":"2026-11-06 12:00:00.000000","expires_on":1000100,"subscription":"x","tenant":"{}","tokenType":"Bearer"}}"#,
        GUID.to_uppercase()
    );
    let got = parse_az_token(json.as_bytes(), &tenant(GUID), 1_000_000).unwrap();
    assert_eq!(got.credential.secret.expose(), "CANARY-az");
    assert_eq!(got.valid_for, Some(Duration::from_secs(100)));
    // A string epoch (older az) and a missing one (the cache limit applies) both work.
    let text = r#"{"accessToken":"a","expires_on":"1000100","tenant":"t"}"#;
    assert!(
        parse_az_token(text.as_bytes(), &tenant("t.io"), 1_000_000)
            .unwrap()
            .valid_for
            .is_some()
    );
    // An `expires_on` of another type counts as none.
    let odd = r#"{"accessToken":"a","expires_on":true}"#;
    assert_eq!(
        parse_az_token(odd.as_bytes(), &tenant("t.io"), 0)
            .unwrap()
            .valid_for,
        None
    );
    let none = r#"{"accessToken":"a","expiresOn":"2026-11-06 12:00:00.000000"}"#;
    assert_eq!(
        parse_az_token(none.as_bytes(), &tenant("t.io"), 0)
            .unwrap()
            .valid_for,
        None
    );
}

#[test]
fn unix_now_is_the_real_clock() {
    let real = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let got = unix_now();
    assert!(got >= 1_700_000_000, "{got}");
    assert!(got.abs_diff(real) <= 5, "{got} vs {real}");
}

#[test]
fn guid_needs_five_hex_parts_of_the_right_lengths() {
    assert!(is_guid("11111111-2222-3333-4444-555555555555"));
    assert!(is_guid("abcdef01-ABCD-ef01-2345-6789abcdef01"));
    for bad in [
        // Right length and dash count, but not hexadecimal.
        "gggggggg-2222-3333-4444-555555555555",
        "11111111-2222-3333-4444-55555555555z",
        // Right characters and part lengths, but too few or too many parts.
        "11111111-2222-3333-4444",
        "11111111-2222-3333-4444-555555555555-6666",
        // Dashes in the wrong places (hex only, same length).
        "1111111-12222-3333-4444-555555555555",
        "11111111-2222-333-34444-555555555555",
        "11111111-2222-3333-4444-55555555555-5",
        // A domain name, and the empty string.
        "contoso.onmicrosoft.com",
        "",
    ] {
        assert!(!is_guid(bad), "{bad}");
    }
}

#[test]
fn az_answer_for_another_tenant_or_unreadable_is_refused() {
    let other = r#"{"accessToken":"a","expires_on":9999999999,"tenant":"11111111-2222-3333-4444-555555555555"}"#;
    assert_eq!(
        parse_az_token(other.as_bytes(), &tenant(GUID), 0).unwrap_err(),
        SourceError::WrongTenant
    );
    let missing = r#"{"accessToken":"a","expires_on":9999999999}"#;
    assert_eq!(
        parse_az_token(missing.as_bytes(), &tenant(GUID), 0).unwrap_err(),
        SourceError::WrongTenant
    );
    for (bad, why) in [
        ("not json", Refusal::Unreadable),
        ("{}", Refusal::Unreadable),
        (r#"{"accessToken":5}"#, Refusal::Unreadable),
        (r#"{"accessToken":"a b"}"#, Refusal::Malformed),
        (r#"{"accessToken":"a\nb"}"#, Refusal::Malformed),
    ] {
        assert_eq!(
            parse_az_token(bad.as_bytes(), &tenant("t.io"), 0).unwrap_err(),
            SourceError::Refused(why),
            "{bad}"
        );
    }
    let past = r#"{"accessToken":"a","expires_on":5}"#;
    assert_eq!(
        parse_az_token(past.as_bytes(), &tenant("t.io"), 10).unwrap_err(),
        SourceError::NotSignedIn
    );
}

#[test]
fn az_failures_name_the_missing_piece() {
    for (stderr, want) in [
        (
            "ERROR: Please run 'az login' to setup account.",
            SourceError::NotSignedIn,
        ),
        ("ERROR: No accounts were found", SourceError::NotSignedIn),
        (
            "AADSTS70043: The refresh token has expired",
            SourceError::NotSignedIn,
        ),
        (
            "AADSTS50076: MFA needed. Interactive authentication is needed.",
            SourceError::NotSignedIn,
        ),
        (
            "AADSTS90002: Tenant 'x' not found.",
            SourceError::WrongTenant,
        ),
        (
            "AADSTS50020: User account from another identity provider",
            SourceError::WrongTenant,
        ),
        (
            "ERROR: Tenant 'abc' is not found in the account",
            SourceError::WrongTenant,
        ),
        (
            "AADSTS65001: The user has not consented",
            SourceError::NoAccess,
        ),
        (
            "AADSTS500011: resource principal not found",
            SourceError::NoAccess,
        ),
        // The message Entra gives for this code, whole: its words also fit a wrong tenant.
        (
            "ERROR: AADSTS500011: The resource principal named 499b84ac-1321-427f-aa17-267ca6975798 was not found in the tenant named Contoso. This can happen if the application has not been installed by the administrator of the tenant or consented to by any user in the tenant. You might have sent your authentication request to the wrong tenant. Trace ID: 0 Correlation ID: 0 Timestamp: 2026-11-12 19:00:00Z",
            SourceError::NoAccess,
        ),
        // A code names its case even when the text around it suggests a fresh login.
        (
            "AADSTS50020: User account from another identity provider does not exist in tenant. Run 'az login'.",
            SourceError::WrongTenant,
        ),
        (
            "ERROR: Tenant 'abc' is not found in the account. Please run 'az login'.",
            SourceError::WrongTenant,
        ),
        (
            "ERROR: The user does not have access to this organization",
            SourceError::NoAccess,
        ),
        // Each half of the tenant wording alone says nothing about the tenant.
        (
            "ERROR: Please run 'az login --tenant contoso.example' to setup account.",
            SourceError::NotSignedIn,
        ),
        (
            "ERROR: The subscription 'x' was not found.",
            SourceError::CouldNotRun(Tool::Az),
        ),
        (
            "Traceback (most recent call last): boom",
            SourceError::CouldNotRun(Tool::Az),
        ),
        ("", SourceError::CouldNotRun(Tool::Az)),
    ] {
        assert_eq!(classify_az_failure(stderr), want, "{stderr}");
    }
}

#[test]
fn pasted_tokens_are_trimmed_and_checked() {
    assert_eq!(pasted_token("  abc\n").unwrap().expose(), "abc");
    assert!(pasted_token("   ").is_err());
    assert!(pasted_token("a b").is_err());
    assert!(pasted_token("a\nb").is_err());
    assert!(pasted_token(&"a".repeat(9000)).is_err());
}

#[test]
fn a_token_of_8192_bytes_is_taken_and_one_byte_more_is_not() {
    assert_eq!(
        token_secret(&"a".repeat(8192), false)
            .unwrap()
            .expose()
            .len(),
        8192
    );
    assert_eq!(
        token_secret(&"a".repeat(8193), false).unwrap_err(),
        SourceError::Refused(Refusal::Malformed)
    );
}
