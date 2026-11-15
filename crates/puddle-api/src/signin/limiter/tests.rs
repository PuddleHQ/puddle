// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn fail_times(limiter: &mut Limiter, key: &str, times: u32, now: u64) {
    for _ in 0..times {
        limiter.admit(key, now).unwrap();
        limiter.failed(key, now);
    }
}

#[test]
fn an_account_locks_after_five_wrong_passwords_and_backs_off() {
    let mut limiter = Limiter::default();
    fail_times(&mut limiter, "ann", 4, 0);
    assert!(limiter.admit("ann", 0).is_ok(), "four failures do not lock");
    limiter.failed("ann", 0);
    assert_eq!(
        limiter.admit("ann", 1_000),
        Err(TryLater { after_ms: 29_000 })
    );
    assert!(limiter.admit("ann", BASE_LOCK_MS).is_ok());
    // The next failure doubles the lock.
    limiter.failed("ann", BASE_LOCK_MS);
    assert_eq!(
        limiter.admit("ann", BASE_LOCK_MS),
        Err(TryLater {
            after_ms: 2 * BASE_LOCK_MS
        })
    );
}

#[test]
fn the_lock_never_exceeds_its_ceiling() {
    let mut limiter = Limiter::default();
    for _ in 0..200 {
        limiter.failed("ann", 0);
    }
    assert_eq!(lock_ms(200), MAX_LOCK_MS);
    assert_eq!(
        limiter.admit("ann", 0),
        Err(TryLater {
            after_ms: MAX_LOCK_MS
        })
    );
    assert!(limiter.admit("ann", MAX_LOCK_MS).is_ok());
}

#[test]
fn locking_one_account_does_not_lock_another() {
    let mut limiter = Limiter::default();
    fail_times(&mut limiter, "ann", 5, 0);
    assert!(limiter.admit("ann", 0).is_err());
    assert!(limiter.admit("bob", 0).is_ok());
}

#[test]
fn a_right_password_forgives_the_failures_before_it() {
    let mut limiter = Limiter::default();
    fail_times(&mut limiter, "ann", 4, 0);
    limiter.succeeded("ann");
    fail_times(&mut limiter, "ann", 4, 1);
    assert!(limiter.admit("ann", 1).is_ok());
}

#[test]
fn an_unknown_name_is_locked_like_a_known_one() {
    let mut limiter = Limiter::default();
    fail_times(&mut limiter, "nobody", 5, 0);
    assert!(limiter.admit("nobody", 0).is_err());
}

#[test]
fn only_thirty_checks_happen_in_any_minute_whoever_asks() {
    let mut limiter = Limiter::default();
    for i in 0..PER_MINUTE {
        // Different names, so no account lock is involved.
        limiter.admit(&format!("user{i}"), 10_000).unwrap();
    }
    let later = limiter.admit("another", 10_000).unwrap_err();
    assert_eq!(later.after_ms, MINUTE_MS);
    assert_eq!(later.seconds(), 60);
    assert!(limiter.admit_any(10_000 + MINUTE_MS - 1).is_err());
    assert!(limiter.admit_any(10_000 + MINUTE_MS).is_ok());
}

#[test]
fn retry_after_is_at_least_a_second() {
    assert_eq!(TryLater { after_ms: 0 }.seconds(), 1);
    assert_eq!(TryLater { after_ms: 1 }.seconds(), 1);
    assert_eq!(TryLater { after_ms: 1_001 }.seconds(), 2);
}

#[test]
fn the_tracked_accounts_are_bounded_and_the_oldest_go_first() {
    let mut limiter = Limiter::default();
    for i in 0..MAX_TRACKED {
        limiter.failed(&format!("user{i}"), i as u64);
    }
    limiter.failed("newcomer", MAX_TRACKED as u64);
    assert_eq!(limiter.accounts.len(), MAX_TRACKED);
    assert!(!limiter.accounts.contains_key("user0"));
    assert!(limiter.accounts.contains_key("newcomer"));
}
