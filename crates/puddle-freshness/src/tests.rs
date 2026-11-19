// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

const NOW: u64 = 1_800_000_000;
const DAY: u64 = 24 * 60 * 60;
const POLICY: Policy<'static> = Policy {
    project: "puddle",
    max_validity_secs: 14 * DAY,
    sequence_ceiling: 1_000_000_000,
};
const OTHER_SEED: [u8; 32] = [0x51; 32];

fn keys() -> Vec<VerifyingKey> {
    vec![public_key(&PLACEHOLDER_SEED)]
}

fn statement(sequence: u64, issued_at: u64, expires_at: u64) -> Statement {
    Statement {
        version: FORMAT_VERSION,
        project: "puddle".into(),
        sequence,
        issued_at,
        expires_at,
    }
}

fn file(sequence: u64, issued_at: u64, expires_at: u64) -> Vec<u8> {
    sign(
        &PLACEHOLDER_SEED,
        &statement(sequence, issued_at, expires_at),
    )
}

fn fresh(sequence: u64) -> Vec<u8> {
    file(sequence, NOW - DAY, NOW + 6 * DAY)
}

fn check(bytes: &[u8], last: Option<&Statement>) -> Result<Statement, FreshnessError> {
    verify(bytes, &keys(), &POLICY, NOW, last)
}

#[test]
fn a_fresh_file_is_accepted_and_returned() {
    let got = check(&fresh(7), None).unwrap();
    assert_eq!(got, statement(7, NOW - DAY, NOW + 6 * DAY));
}

#[test]
fn the_stored_fixture_still_verifies_and_is_reproduced_byte_for_byte() {
    let fixture = include_bytes!("../tests/fixtures/valid.json");
    let got = check(fixture, None).unwrap();
    assert_eq!(got.sequence, 42);
    // Ed25519 is deterministic: signing the same statement gives the same bytes, so a change to
    // the format or the signed text shows up here before a client sees it.
    assert_eq!(sign(&PLACEHOLDER_SEED, &got), fixture.as_slice());
}

#[test]
fn an_expired_file_is_refused() {
    let bytes = file(5, NOW - 8 * DAY, NOW - DAY);
    assert_eq!(
        check(&bytes, None),
        Err(FreshnessError::Expired {
            expires_at: NOW - DAY,
            now: NOW
        })
    );
}

#[test]
fn the_expiry_second_itself_is_expired() {
    let bytes = file(5, NOW - DAY, NOW);
    assert!(matches!(
        check(&bytes, None),
        Err(FreshnessError::Expired { .. })
    ));
    let last = file(5, NOW - DAY, NOW + 1);
    assert!(check(&last, None).is_ok());
}

#[test]
fn a_replayed_lower_sequence_is_refused() {
    let seen = check(&fresh(9), None).unwrap();
    assert_eq!(
        check(&fresh(8), Some(&seen)),
        Err(FreshnessError::SequenceRollback { seen: 9, got: 8 })
    );
}

#[test]
fn the_same_file_again_and_a_higher_sequence_are_accepted() {
    let seen = check(&fresh(9), None).unwrap();
    assert_eq!(check(&fresh(9), Some(&seen)).unwrap(), seen);
    assert_eq!(check(&fresh(10), Some(&seen)).unwrap().sequence, 10);
}

#[test]
fn the_same_sequence_with_other_content_is_refused() {
    let seen = check(&fresh(9), None).unwrap();
    let other = file(9, NOW - DAY, NOW + 5 * DAY);
    assert_eq!(
        check(&other, Some(&seen)),
        Err(FreshnessError::SequenceConflict(9))
    );
}

#[test]
fn a_signature_from_an_untrusted_key_is_refused() {
    let bytes = sign(&OTHER_SEED, &statement(1, NOW - DAY, NOW + DAY));
    assert_eq!(check(&bytes, None), Err(FreshnessError::BadSignature));
    assert_eq!(
        verify(&bytes, &[], &POLICY, NOW, None),
        Err(FreshnessError::BadSignature)
    );
}

#[test]
fn any_one_of_several_trusted_keys_is_enough() {
    let bytes = sign(&OTHER_SEED, &statement(1, NOW - DAY, NOW + DAY));
    let both = [public_key(&PLACEHOLDER_SEED), public_key(&OTHER_SEED)];
    assert!(verify(&bytes, &both, &POLICY, NOW, None).is_ok());
}

#[test]
fn a_changed_statement_breaks_the_signature() {
    let text = String::from_utf8(fresh(3)).unwrap();
    let tampered = text.replace("\\\"sequence\\\":3", "\\\"sequence\\\":4");
    assert_ne!(tampered, text, "the fixture text must contain the field");
    assert_eq!(
        check(tampered.as_bytes(), None),
        Err(FreshnessError::BadSignature)
    );
}

#[test]
fn a_truncated_file_is_malformed_at_every_length() {
    let bytes = fresh(3);
    for cut in 0..bytes.len() - 1 {
        // The last byte is only the trailing newline, so every shorter prefix is cut inside the JSON.
        assert!(
            matches!(
                check(&bytes[..cut], None),
                Err(FreshnessError::Malformed(_))
            ),
            "prefix of {cut} bytes"
        );
    }
}

#[test]
fn bytes_that_are_not_a_freshness_file_are_malformed() {
    let good = String::from_utf8(fresh(3)).unwrap();
    let envelope = |signed: &str, signature: &str| {
        serde_json::to_vec(&serde_json::json!({"signed": signed, "signature": signature})).unwrap()
    };
    for bad in [
        b"".to_vec(),
        b"not json".to_vec(),
        b"[]".to_vec(),
        br#"{"signed":"x"}"#.to_vec(),
        format!(r#"{{"extra":1,{}"#, &good[1..]).into_bytes(),
        envelope("{}", "!!!not base64!!!"),
        envelope("{}", &STANDARD.encode([1_u8; 10])),
    ] {
        assert!(
            matches!(check(&bad, None), Err(FreshnessError::Malformed(_))),
            "{}",
            String::from_utf8_lossy(&bad)
        );
    }
    // Well-formed but wrong signature bytes are a bad signature, not a malformed file.
    let zeros = envelope("{}", &STANDARD.encode([0_u8; 64]));
    assert_eq!(check(&zeros, None), Err(FreshnessError::BadSignature));
    // A correct signature over text that is not a statement is malformed too, not a bad signature.
    let signed = "not a statement";
    let sig = SigningKey::from_bytes(&PLACEHOLDER_SEED).sign(signed.as_bytes());
    let bytes = envelope(signed, &STANDARD.encode(sig.to_bytes()));
    assert!(matches!(
        check(&bytes, None),
        Err(FreshnessError::Malformed(m)) if m.starts_with("statement")
    ));
}

#[test]
fn another_format_version_project_or_window_is_refused() {
    let mut s = statement(1, NOW - DAY, NOW + DAY);
    s.version = 2;
    assert_eq!(
        check(&sign(&PLACEHOLDER_SEED, &s), None),
        Err(FreshnessError::UnsupportedVersion(2))
    );
    let mut s = statement(1, NOW - DAY, NOW + DAY);
    s.project = "other".into();
    assert_eq!(
        check(&sign(&PLACEHOLDER_SEED, &s), None),
        Err(FreshnessError::WrongProject {
            expected: "puddle".into(),
            found: "other".into()
        })
    );
    assert_eq!(
        check(&file(1, NOW, NOW), None),
        Err(FreshnessError::EmptyWindow {
            issued_at: NOW,
            expires_at: NOW
        })
    );
}

#[test]
fn a_window_over_the_cap_a_future_issue_and_a_high_sequence_are_refused() {
    assert_eq!(
        check(&file(1, NOW - DAY, NOW + 30 * DAY), None),
        Err(FreshnessError::ValidityTooLong {
            secs: 31 * DAY,
            max: 14 * DAY
        })
    );
    assert_eq!(
        check(&file(1, NOW + DAY, NOW + 2 * DAY), None),
        Err(FreshnessError::NotYetValid {
            issued_at: NOW + DAY,
            now: NOW
        })
    );
    // Within the skew it is fine.
    assert!(check(&file(1, NOW + CLOCK_SKEW_SECS, NOW + DAY), None).is_ok());
    assert_eq!(
        check(&fresh(u64::MAX), None),
        Err(FreshnessError::SequenceCeiling {
            ceiling: 1_000_000_000,
            got: u64::MAX
        })
    );
}

#[test]
fn refresh_starts_at_the_minute_count_and_counts_up() {
    let first = refresh(
        None,
        &PLACEHOLDER_SEED,
        "puddle",
        NOW,
        DEFAULT_VALIDITY_SECS,
    )
    .unwrap()
    .bytes;
    let a = check(&first, None).unwrap();
    assert_eq!(
        (a.sequence, a.expires_at - a.issued_at),
        (NOW / 60, 7 * DAY)
    );
    let second = refresh(
        Some(&first),
        &PLACEHOLDER_SEED,
        "puddle",
        NOW + DAY,
        DEFAULT_VALIDITY_SECS,
    )
    .unwrap()
    .bytes;
    let b = verify(&second, &keys(), &POLICY, NOW + DAY, Some(&a)).unwrap();
    assert_eq!((b.sequence, b.issued_at), (NOW / 60 + 1, NOW + DAY));
}

#[test]
fn a_fresh_start_after_a_lost_file_is_above_every_sequence_given_out_before() {
    // A year of daily refreshes, then the file is lost and the job starts over.
    let mut file = refresh(None, &PLACEHOLDER_SEED, "puddle", NOW, DAY)
        .unwrap()
        .bytes;
    for day in 1..=365 {
        file = refresh(
            Some(&file),
            &PLACEHOLDER_SEED,
            "puddle",
            NOW + day * DAY,
            DAY,
        )
        .unwrap()
        .bytes;
    }
    let last = verify(&file, &keys(), &POLICY, NOW + 365 * DAY, None).unwrap();
    let restart = refresh(None, &PLACEHOLDER_SEED, "puddle", NOW + 366 * DAY, DAY)
        .unwrap()
        .bytes;
    let got = verify(&restart, &keys(), &POLICY, NOW + 366 * DAY, Some(&last)).unwrap();
    assert!(got.sequence > last.sequence, "{got:?} after {last:?}");
}

#[test]
fn refresh_recovers_after_the_previous_file_expired() {
    let old = refresh(None, &PLACEHOLDER_SEED, "puddle", NOW, DAY)
        .unwrap()
        .bytes;
    let later = refresh(Some(&old), &PLACEHOLDER_SEED, "puddle", NOW + 30 * DAY, DAY)
        .unwrap()
        .bytes;
    let got = verify(&later, &keys(), &POLICY, NOW + 30 * DAY, None).unwrap();
    assert_eq!(got.sequence, NOW / 60 + 1);
}

#[test]
fn refresh_never_restarts_over_a_file_it_cannot_trust() {
    let theirs = sign(&OTHER_SEED, &statement(40, NOW - DAY, NOW + DAY));
    for previous in [theirs.as_slice(), b"garbage".as_slice()] {
        assert!(refresh(Some(previous), &PLACEHOLDER_SEED, "puddle", NOW, DAY).is_err());
    }
}

#[test]
fn refresh_stops_at_the_largest_sequence() {
    let top = sign(
        &PLACEHOLDER_SEED,
        &statement(u64::MAX, NOW - DAY, NOW + DAY),
    );
    assert_eq!(
        refresh(Some(&top), &PLACEHOLDER_SEED, "puddle", NOW, DAY),
        Err(FreshnessError::SequenceOverflow(u64::MAX))
    );
}

#[test]
fn the_placeholder_key_is_not_a_key_anyone_else_has() {
    assert_ne!(public_key(&PLACEHOLDER_SEED), public_key(&OTHER_SEED));
}
