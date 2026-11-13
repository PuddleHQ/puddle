// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn device(n: i64) -> DeviceId {
    DeviceId::from_stored(n)
}

#[test]
fn a_secret_is_forty_three_url_safe_characters_and_never_repeats() {
    let a = random_secret().unwrap();
    let b = random_secret().unwrap();
    assert_ne!(*a, *b);
    assert_eq!(a.len(), 43);
    assert!(
        a.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    );
}

#[test]
fn the_digest_is_not_the_secret_and_is_stable() {
    let secret = random_secret().unwrap();
    assert_eq!(digest(&secret), digest(&secret));
    assert_ne!(digest(&secret), digest("other"));
    assert!(
        !digest(&secret)
            .windows(8)
            .any(|w| secret.as_bytes().windows(8).any(|s| s == w))
    );
}

#[test]
fn an_access_token_works_for_fifteen_minutes_then_not() {
    let tokens = AccessTokens::default();
    let (token, expires_at) = tokens.issue(device(1), 1_000).unwrap();
    assert_eq!(expires_at, 1_000 + ACCESS_TTL_MS);
    assert_eq!(tokens.device_of(&token, 1_000), Some(device(1)));
    assert_eq!(tokens.device_of(&token, expires_at - 1), Some(device(1)));
    assert_eq!(tokens.device_of(&token, expires_at), None);
    assert_eq!(tokens.device_of("nonsense", 1_000), None);
}

#[test]
fn revoking_a_device_ends_its_tokens_and_only_its_tokens() {
    let tokens = AccessTokens::default();
    let (one, _) = tokens.issue(device(1), 0).unwrap();
    let (one_more, _) = tokens.issue(device(1), 1).unwrap();
    let (two, _) = tokens.issue(device(2), 0).unwrap();
    tokens.revoke(device(1));
    assert_eq!(tokens.device_of(&one, 2), None);
    assert_eq!(tokens.device_of(&one_more, 2), None);
    assert_eq!(tokens.device_of(&two, 2), Some(device(2)));
}

#[test]
fn a_device_keeps_only_its_newest_few_tokens() {
    let tokens = AccessTokens::default();
    let issued: Vec<_> = (0..PER_DEVICE as u64 + 2)
        .map(|i| tokens.issue(device(1), i).unwrap().0)
        .collect();
    let now = 100;
    let alive = issued
        .iter()
        .filter(|t| tokens.device_of(t, now).is_some())
        .count();
    assert_eq!(alive, PER_DEVICE);
    assert!(
        tokens.device_of(&issued[0], now).is_none(),
        "the oldest went"
    );
    assert!(tokens.device_of(issued.last().unwrap(), now).is_some());
}

#[test]
fn expired_tokens_are_dropped_when_a_new_one_is_made() {
    let tokens = AccessTokens::default();
    let (old, _) = tokens.issue(device(1), 0).unwrap();
    tokens.issue(device(2), ACCESS_TTL_MS + 1).unwrap();
    let live = tokens.live.lock().unwrap();
    assert!(!live.contains_key(&digest(&old)));
}
