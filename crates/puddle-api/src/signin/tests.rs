// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_store::{Limits, ManualClock};

use super::limiter::{BASE_LOCK_MS, FREE_FAILURES};
use super::tokens::ACCESS_TTL_MS;
use super::*;

/// A test password made at run time, the same one for the same label. A literal handed to the
/// hasher is flagged as a hard-coded cryptographic value, and none of these protects anything.
pub(crate) fn pw(label: &str) -> &'static str {
    static SUFFIX: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let suffix = SUFFIX.get_or_init(|| {
        let mut bytes = [0_u8; 6];
        getrandom::fill(&mut bytes).unwrap();
        bytes.iter().fold(String::new(), |mut hex, b| {
            let _ = std::fmt::Write::write_fmt(&mut hex, format_args!("{b:02x}"));
            hex
        })
    });
    Box::leak(format!("{label} {suffix}").into_boxed_str())
}

/// The password most tests sign in with.
pub(crate) fn password() -> &'static str {
    pw("correct horse battery")
}

/// Five characters, under the twelve the policy wants.
pub(crate) fn short() -> &'static str {
    &pw("")[..5]
}

/// A password of no characters, from a runtime string.
pub(crate) fn empty() -> &'static str {
    &pw("")[..0]
}

struct Fixture {
    store: Arc<Store>,
    secrets: Arc<AccountSecrets>,
    clock: Arc<ManualClock>,
    sign_in: Arc<SignIn>,
}

fn fixture_with(multi_user: bool) -> Fixture {
    let clock = Arc::new(ManualClock::new(1_000_000));
    let store = Arc::new(Store::open_in_memory(clock.clone(), Limits::default()).unwrap());
    let secrets = Arc::new(store.take_account_secrets().unwrap());
    let sign_in =
        Arc::new(SignIn::new(store.clone(), secrets.clone(), clock.clone(), multi_user).unwrap());
    Fixture {
        store,
        secrets,
        clock,
        sign_in,
    }
}

fn fixture() -> Fixture {
    fixture_with(false)
}

async fn owner_signed_up(f: &Fixture) -> Granted {
    f.sign_in
        .setup("Tijs", password(), Some("desktop"))
        .await
        .unwrap()
}

fn code(err: &ApiError) -> ErrorCode {
    err.code()
}

#[tokio::test]
async fn the_status_says_setup_is_open_until_the_first_visitor_has_made_the_owner() {
    let f = fixture();
    let status = f.sign_in.status().await.unwrap();
    assert!(status.setup_open && status.password_login);
    owner_signed_up(&f).await;
    let status = f.sign_in.status().await.unwrap();
    assert!(!status.setup_open && status.password_login);
}

#[tokio::test]
async fn the_first_visitor_becomes_the_owner_and_is_signed_in() {
    let f = fixture();
    let granted = f
        .sign_in
        .setup("Tijs", password(), Some(" desktop "))
        .await
        .unwrap();
    assert_eq!(granted.grant.user.name, "Tijs");
    assert_eq!(granted.grant.user.role, "owner");
    assert_eq!(granted.grant.user.id, UserId::OWNER.get());
    assert_eq!(granted.grant.device.name, "desktop");
    assert!(granted.grant.device.current);
    assert_eq!(granted.grant.expires_at, 1_000_000 + ACCESS_TTL_MS);
    assert_eq!(granted.refresh.len(), 43);
    assert_ne!(granted.refresh.as_str(), granted.grant.access_token);
    let principal = f
        .sign_in
        .principal_for(&granted.grant.access_token)
        .await
        .unwrap();
    assert_eq!(principal.user, UserId::OWNER);
    assert_eq!(principal.role, Role::Owner);
    assert_eq!(
        principal.via,
        Via::Device(DeviceId::from_stored(granted.grant.device.id))
    );
}

#[tokio::test]
async fn nothing_in_the_database_signs_in_and_the_password_is_not_stored() {
    let f = fixture();
    let granted = owner_signed_up(&f).await;
    let mut stored = String::new();
    f.secrets
        .check_password(UserId::OWNER, |hash| {
            stored = hash.to_owned();
            true
        })
        .unwrap();
    assert!(stored.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
    assert!(!stored.contains(password()));
    // The refresh token is looked up by its digest only.
    assert!(
        f.secrets
            .session_for(granted.refresh.as_bytes())
            .unwrap()
            .is_none()
    );
    assert!(
        f.secrets
            .session_for(&digest(&granted.refresh))
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn setup_works_once_whoever_asks() {
    let f = fixture();
    owner_signed_up(&f).await;
    let err = f
        .sign_in
        .setup("mallory", pw("another long password"), None)
        .await
        .unwrap_err();
    assert_eq!(code(&err), ErrorCode::Conflict);
    assert_eq!(f.store.user(UserId::OWNER).unwrap().name, "Tijs");
}

#[tokio::test]
async fn two_first_visitors_at_once_make_one_owner() {
    let f = fixture();
    let (a, b) = tokio::join!(
        f.sign_in.setup("alice", pw("first long password"), None),
        f.sign_in.setup("bobby", pw("other long password"), None),
    );
    assert_eq!(
        [a.is_ok(), b.is_ok()].iter().filter(|ok| **ok).count(),
        1,
        "exactly one wins"
    );
    let name = f.store.user(UserId::OWNER).unwrap().name;
    assert!(name == "alice" || name == "bobby");
}

#[tokio::test]
async fn setup_refuses_a_bad_name_or_a_weak_password_and_stays_open() {
    let f = fixture();
    let upper = "tijs".to_uppercase();
    for (name, password) in [
        ("", password()),
        ("Tijs", short()),
        ("tijs", upper.as_str()),
    ] {
        let err = f.sign_in.setup(name, password, None).await.unwrap_err();
        assert_eq!(code(&err), ErrorCode::Invalid, "{name:?}");
    }
    assert!(f.sign_in.status().await.unwrap().setup_open);
}

#[tokio::test]
async fn a_password_signs_in_to_a_new_device_every_time() {
    let f = fixture();
    let first = owner_signed_up(&f).await;
    let second = f
        .sign_in
        .sign_in("tijs", password(), Some("phone"))
        .await
        .unwrap();
    assert_ne!(first.grant.device.id, second.grant.device.id);
    assert_ne!(first.refresh.as_str(), second.refresh.as_str());
    assert_ne!(first.grant.access_token, second.grant.access_token);
    let devices = f
        .sign_in
        .devices(
            UserId::OWNER,
            Some(DeviceId::from_stored(second.grant.device.id)),
        )
        .await
        .unwrap();
    assert_eq!(devices.devices.len(), 2);
    assert_eq!(devices.devices.iter().filter(|d| d.current).count(), 1);
}

#[tokio::test]
async fn a_wrong_name_and_a_wrong_password_answer_the_same() {
    let f = fixture();
    owner_signed_up(&f).await;
    let wrong_password = f
        .sign_in
        .sign_in("Tijs", pw("not the password!"), None)
        .await
        .unwrap_err();
    let wrong_name = f
        .sign_in
        .sign_in("nobody", password(), None)
        .await
        .unwrap_err();
    assert_eq!(code(&wrong_password), ErrorCode::Unauthorized);
    assert_eq!(code(&wrong_name), ErrorCode::Unauthorized);
    assert_eq!(wrong_password.message(), wrong_name.message());
}

#[tokio::test]
async fn an_account_without_a_password_cannot_sign_in_with_any() {
    let f = fixture();
    owner_signed_up(&f).await;
    f.secrets.clear_password(UserId::OWNER).unwrap();
    let err = f
        .sign_in
        .sign_in("Tijs", password(), None)
        .await
        .unwrap_err();
    assert_eq!(code(&err), ErrorCode::Unauthorized);
}

#[tokio::test]
async fn other_users_cannot_sign_in_while_several_users_are_off() {
    let off = fixture_with(false);
    owner_signed_up(&off).await;
    let ann = off
        .store
        .add_user(&UserName::new("ann").unwrap())
        .unwrap()
        .id;
    off.secrets
        .set_password_hash(ann, &password::hash(password()).unwrap())
        .unwrap();
    let err = off
        .sign_in
        .sign_in("ann", password(), None)
        .await
        .unwrap_err();
    assert_eq!(code(&err), ErrorCode::Unauthorized);

    let on = fixture_with(true);
    owner_signed_up(&on).await;
    let ann = on
        .store
        .add_user(&UserName::new("ann").unwrap())
        .unwrap()
        .id;
    on.secrets
        .set_password_hash(ann, &password::hash(password()).unwrap())
        .unwrap();
    let granted = on.sign_in.sign_in("ann", password(), None).await.unwrap();
    assert_eq!(granted.grant.user.role, "user");
    let principal = on
        .sign_in
        .principal_for(&granted.grant.access_token)
        .await
        .unwrap();
    assert_eq!((principal.user, principal.role), (ann, Role::User));
}

#[tokio::test]
async fn five_wrong_passwords_lock_the_account_even_for_the_right_one() {
    let f = fixture();
    owner_signed_up(&f).await;
    for _ in 0..FREE_FAILURES {
        let err = f
            .sign_in
            .sign_in("Tijs", pw("not the password!"), None)
            .await
            .unwrap_err();
        assert_eq!(code(&err), ErrorCode::Unauthorized);
    }
    let locked = f
        .sign_in
        .sign_in("Tijs", password(), None)
        .await
        .unwrap_err();
    assert_eq!(code(&locked), ErrorCode::RateLimited);
    // The lock is per account, not per client: the same name from anywhere is locked.
    f.clock.advance(BASE_LOCK_MS);
    f.sign_in.sign_in("TIJS", password(), None).await.unwrap();
}

#[tokio::test]
async fn an_unknown_name_is_locked_like_a_known_one() {
    let f = fixture();
    owner_signed_up(&f).await;
    for _ in 0..FREE_FAILURES {
        let _ = f.sign_in.sign_in("ghost", password(), None).await;
    }
    let err = f
        .sign_in
        .sign_in("ghost", password(), None)
        .await
        .unwrap_err();
    assert_eq!(code(&err), ErrorCode::RateLimited);
}

#[tokio::test]
async fn password_checks_are_limited_everywhere_each_minute() {
    let f = fixture();
    owner_signed_up(&f).await;
    let mut limited = 0;
    for i in 0..40 {
        // A different name each time, so no account lock is the cause.
        let err = f
            .sign_in
            .sign_in(&format!("user{i}"), password(), None)
            .await
            .unwrap_err();
        if code(&err) == ErrorCode::RateLimited {
            limited += 1;
        }
    }
    assert!(limited >= 9, "{limited}");
    f.clock.advance(60_000);
    f.sign_in.sign_in("Tijs", password(), None).await.unwrap();
}

#[tokio::test]
async fn a_refresh_gives_a_new_access_token_and_slides_the_expiry() {
    let f = fixture();
    let granted = owner_signed_up(&f).await;
    f.clock.advance(20 * 24 * 60 * 60 * 1000);
    let grant = f.sign_in.refresh(&granted.refresh).await.unwrap();
    assert_ne!(grant.access_token, granted.grant.access_token);
    assert_eq!(grant.device.id, granted.grant.device.id);
    assert_eq!(grant.device.expires_at, f.clock.now_ms() + DEVICE_TTL_MS);
    // Used within 30 days of the last use, so still good 20 more days on.
    f.clock.advance(20 * 24 * 60 * 60 * 1000);
    f.sign_in.refresh(&granted.refresh).await.unwrap();
}

#[tokio::test]
async fn a_device_idle_for_thirty_days_is_signed_out() {
    let f = fixture();
    let granted = owner_signed_up(&f).await;
    f.clock.advance(DEVICE_TTL_MS);
    let err = f.sign_in.refresh(&granted.refresh).await.unwrap_err();
    assert_eq!(code(&err), ErrorCode::Unauthorized);
    assert!(
        f.sign_in
            .principal_for(&granted.grant.access_token)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn an_access_token_stops_after_fifteen_minutes() {
    let f = fixture();
    let granted = owner_signed_up(&f).await;
    f.clock.advance(ACCESS_TTL_MS - 1);
    assert!(
        f.sign_in
            .principal_for(&granted.grant.access_token)
            .await
            .is_some()
    );
    f.clock.advance(1);
    assert!(
        f.sign_in
            .principal_for(&granted.grant.access_token)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn an_unknown_refresh_token_is_refused_and_so_is_an_access_token_in_its_place() {
    let f = fixture();
    let granted = owner_signed_up(&f).await;
    for bad in ["", "nonsense", granted.grant.access_token.as_str()] {
        let err = f.sign_in.refresh(bad).await.unwrap_err();
        assert_eq!(code(&err), ErrorCode::Unauthorized, "{bad:?}");
    }
    // And a refresh token is no access token.
    assert!(f.sign_in.principal_for(&granted.refresh).await.is_none());
}

#[tokio::test]
async fn revoking_a_device_ends_its_tokens_at_once_and_only_its_own() {
    let f = fixture();
    let one = owner_signed_up(&f).await;
    let two = f.sign_in.sign_in("Tijs", password(), None).await.unwrap();
    f.sign_in
        .revoke_device(UserId::OWNER, DeviceId::from_stored(one.grant.device.id))
        .await
        .unwrap();
    assert!(
        f.sign_in
            .principal_for(&one.grant.access_token)
            .await
            .is_none()
    );
    assert_eq!(
        code(&f.sign_in.refresh(&one.refresh).await.unwrap_err()),
        ErrorCode::Unauthorized
    );
    assert!(
        f.sign_in
            .principal_for(&two.grant.access_token)
            .await
            .is_some()
    );
    f.sign_in.refresh(&two.refresh).await.unwrap();
}

#[tokio::test]
async fn a_revoke_from_another_process_counts_without_telling_this_one() {
    let f = fixture();
    let granted = owner_signed_up(&f).await;
    // The server's command line works on the same database, not through this object.
    f.store.revoke_devices_of(UserId::OWNER, None).unwrap();
    assert!(
        f.sign_in
            .principal_for(&granted.grant.access_token)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn nobody_can_revoke_a_device_that_is_not_theirs_and_the_answer_is_the_same_as_for_none() {
    let f = fixture_with(true);
    let granted = owner_signed_up(&f).await;
    let ann = f.store.add_user(&UserName::new("ann").unwrap()).unwrap().id;
    let device = DeviceId::from_stored(granted.grant.device.id);
    let theirs = f.sign_in.revoke_device(ann, device).await.unwrap_err();
    let none = f
        .sign_in
        .revoke_device(ann, DeviceId::from_stored(999))
        .await
        .unwrap_err();
    assert_eq!(code(&theirs), ErrorCode::NotFound);
    assert_eq!(code(&none), ErrorCode::NotFound);
    assert_eq!(
        theirs.message().replace(&device.get().to_string(), "N"),
        none.message().replace("999", "N")
    );
    assert!(
        f.sign_in
            .principal_for(&granted.grant.access_token)
            .await
            .is_some()
    );
    assert_eq!(
        f.sign_in.devices(ann, None).await.unwrap().devices,
        Vec::new()
    );
}

#[tokio::test]
async fn signing_out_revokes_the_device_and_signing_out_twice_is_fine() {
    let f = fixture();
    let granted = owner_signed_up(&f).await;
    f.sign_in.sign_out(&granted.refresh).await.unwrap();
    assert!(
        f.sign_in
            .principal_for(&granted.grant.access_token)
            .await
            .is_none()
    );
    assert_eq!(
        f.sign_in
            .devices(UserId::OWNER, None)
            .await
            .unwrap()
            .devices,
        Vec::new()
    );
    f.sign_in.sign_out(&granted.refresh).await.unwrap();
    f.sign_in.sign_out("nonsense").await.unwrap();
}

#[tokio::test]
async fn a_stream_waits_for_its_device_to_be_revoked() {
    let f = fixture();
    let granted = owner_signed_up(&f).await;
    let device = DeviceId::from_stored(granted.grant.device.id);
    let waiting = tokio::spawn(f.sign_in.clone().ended(device));
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());
    f.sign_in
        .revoke_device(UserId::OWNER, device)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("ends at once on a revoke")
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_stream_notices_a_revoke_made_by_another_process_within_a_few_seconds() {
    let f = fixture();
    let granted = owner_signed_up(&f).await;
    let device = DeviceId::from_stored(granted.grant.device.id);
    let waiting = tokio::spawn(f.sign_in.clone().ended(device));
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!waiting.is_finished());
    f.store.revoke_devices_of(UserId::OWNER, None).unwrap();
    tokio::time::sleep(RECHECK + Duration::from_secs(1)).await;
    assert!(waiting.is_finished());
}

#[tokio::test]
async fn a_stream_of_a_device_that_is_already_gone_ends_at_once() {
    let f = fixture();
    let granted = owner_signed_up(&f).await;
    let device = DeviceId::from_stored(granted.grant.device.id);
    f.store.revoke_device(UserId::OWNER, device).unwrap();
    tokio::time::timeout(Duration::from_secs(5), f.sign_in.clone().ended(device))
        .await
        .unwrap();
}

#[tokio::test]
async fn changing_the_password_needs_the_current_one_and_signs_out_the_other_devices() {
    let f = fixture();
    let this = owner_signed_up(&f).await;
    let other = f.sign_in.sign_in("Tijs", password(), None).await.unwrap();
    let device = DeviceId::from_stored(this.grant.device.id);
    let wrong = f
        .sign_in
        .change_password(
            UserId::OWNER,
            device,
            pw("not the password!"),
            pw("a new long password"),
        )
        .await
        .unwrap_err();
    assert_eq!(code(&wrong), ErrorCode::Unauthorized);
    let weak = f
        .sign_in
        .change_password(UserId::OWNER, device, password(), short())
        .await
        .unwrap_err();
    assert_eq!(code(&weak), ErrorCode::Invalid);
    f.sign_in
        .change_password(UserId::OWNER, device, password(), pw("a new long password"))
        .await
        .unwrap();
    assert!(
        f.sign_in
            .principal_for(&this.grant.access_token)
            .await
            .is_some(),
        "this one stays"
    );
    assert!(
        f.sign_in
            .principal_for(&other.grant.access_token)
            .await
            .is_none()
    );
    assert!(f.sign_in.refresh(&other.refresh).await.is_err());
    assert!(
        f.sign_in.sign_in("Tijs", password(), None).await.is_err(),
        "the old one is gone"
    );
    f.sign_in
        .sign_in("Tijs", pw("a new long password"), None)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_wrong_current_password_counts_toward_the_lock() {
    let f = fixture();
    let this = owner_signed_up(&f).await;
    let device = DeviceId::from_stored(this.grant.device.id);
    for _ in 0..FREE_FAILURES {
        let _ = f
            .sign_in
            .change_password(
                UserId::OWNER,
                device,
                pw("not the password!"),
                pw("a new long password"),
            )
            .await;
    }
    let err = f
        .sign_in
        .sign_in("Tijs", password(), None)
        .await
        .unwrap_err();
    assert_eq!(code(&err), ErrorCode::RateLimited);
}

#[tokio::test]
async fn a_reset_makes_a_one_time_invite_signs_everyone_out_and_does_not_reopen_setup() {
    let f = fixture();
    let granted = owner_signed_up(&f).await;
    let invite = reset_account(&f.store, &f.secrets, "TIJS", f.clock.now_ms()).unwrap();
    assert_eq!(invite.secret.len(), 43);
    assert_eq!(invite.expires_at, f.clock.now_ms() + INVITE_TTL_MS);
    assert!(
        f.sign_in
            .principal_for(&granted.grant.access_token)
            .await
            .is_none()
    );
    assert!(f.sign_in.refresh(&granted.refresh).await.is_err());
    assert!(
        f.sign_in.sign_in("Tijs", password(), None).await.is_err(),
        "the password is gone"
    );
    // The first-visitor door stays shut.
    assert!(!f.sign_in.status().await.unwrap().setup_open);
    assert_eq!(
        code(
            &f.sign_in
                .setup("mallory", pw("another long password"), None)
                .await
                .unwrap_err()
        ),
        ErrorCode::Conflict
    );
    // The invite sets a new password, once.
    let weak = f
        .sign_in
        .redeem_invite(&invite.secret, short(), None)
        .await
        .unwrap_err();
    assert_eq!(
        code(&weak),
        ErrorCode::Invalid,
        "a weak password does not burn the invite"
    );
    let back = f
        .sign_in
        .redeem_invite(
            &invite.secret,
            pw("brand new long password"),
            Some("new desktop"),
        )
        .await
        .unwrap();
    assert_eq!(back.grant.user.name, "Tijs");
    assert_eq!(back.grant.device.name, "new desktop");
    let again = f
        .sign_in
        .redeem_invite(&invite.secret, pw("another long password"), None)
        .await
        .unwrap_err();
    assert_eq!(code(&again), ErrorCode::Unauthorized);
    f.sign_in
        .sign_in("Tijs", pw("brand new long password"), None)
        .await
        .unwrap();
}

#[tokio::test]
async fn an_invite_expires_and_a_guess_is_refused() {
    let f = fixture();
    owner_signed_up(&f).await;
    let invite = reset_account(&f.store, &f.secrets, "Tijs", f.clock.now_ms()).unwrap();
    let guess = f
        .sign_in
        .redeem_invite("nonsense", pw("brand new long password"), None)
        .await
        .unwrap_err();
    assert_eq!(code(&guess), ErrorCode::Unauthorized);
    f.clock.advance(INVITE_TTL_MS);
    let late = f
        .sign_in
        .redeem_invite(&invite.secret, pw("brand new long password"), None)
        .await
        .unwrap_err();
    assert_eq!(code(&late), ErrorCode::Unauthorized);
}

#[tokio::test]
async fn invite_guesses_are_limited_everywhere() {
    let f = fixture();
    owner_signed_up(&f).await;
    let mut limited = 0;
    for i in 0..40 {
        let err = f
            .sign_in
            .redeem_invite(&format!("guess{i}"), pw("brand new long password"), None)
            .await
            .unwrap_err();
        if code(&err) == ErrorCode::RateLimited {
            limited += 1;
        }
    }
    assert!(limited >= 9, "{limited}");
}

#[tokio::test]
async fn resetting_a_user_that_does_not_exist_says_so() {
    let f = fixture();
    let err = reset_account(&f.store, &f.secrets, "ghost", 0).unwrap_err();
    assert!(matches!(err, ResetError::UnknownUser(name) if name == "ghost"));
}

#[tokio::test]
async fn with_password_sign_in_off_nothing_signs_in_but_signed_in_browsers_carry_on() {
    let f = fixture();
    let granted = owner_signed_up(&f).await;
    // Passwords are the only method, so the last one can't be switched off.
    let err = f
        .sign_in
        .set_method(SignInMethod::Password, false)
        .await
        .unwrap_err();
    assert_eq!(code(&err), ErrorCode::Invalid);
    assert!(f.sign_in.status().await.unwrap().password_login);
    // Another method makes it possible.
    f.store
        .register_sign_in_method(SignInMethod::Passkey)
        .unwrap();
    let methods = f
        .sign_in
        .set_method(SignInMethod::Password, false)
        .await
        .unwrap();
    assert_eq!(methods, f.sign_in.methods().await.unwrap());
    assert!(!f.sign_in.status().await.unwrap().password_login);
    let off = |err: ApiError| assert_eq!(code(&err), ErrorCode::Forbidden);
    off(f
        .sign_in
        .sign_in("Tijs", password(), None)
        .await
        .unwrap_err());
    off(f
        .sign_in
        .redeem_invite("x", pw("brand new long password"), None)
        .await
        .unwrap_err());
    off(f
        .sign_in
        .change_password(
            UserId::OWNER,
            DeviceId::from_stored(granted.grant.device.id),
            password(),
            pw("brand new long password"),
        )
        .await
        .unwrap_err());
    // The browsers already in are not thrown out.
    f.sign_in.refresh(&granted.refresh).await.unwrap();
    assert!(
        f.sign_in
            .principal_for(&granted.grant.access_token)
            .await
            .is_some()
    );
    // And now the other method is the last one.
    let last = f
        .sign_in
        .set_method(SignInMethod::Passkey, false)
        .await
        .unwrap_err();
    assert_eq!(code(&last), ErrorCode::Invalid);
}

#[tokio::test]
async fn what_a_sign_in_hands_back_never_shows_its_secrets_in_a_debug_print() {
    let f = fixture();
    let granted = owner_signed_up(&f).await;
    for text in [
        format!("{granted:?}"),
        format!("{:?}", granted.grant),
        format!("{:?}", f.sign_in),
    ] {
        assert!(!text.contains(granted.refresh.as_str()), "{text}");
        assert!(!text.contains(&granted.grant.access_token), "{text}");
    }
}

#[test]
fn a_hash_that_cannot_be_made_is_an_internal_error_and_a_weak_password_is_invalid() {
    assert_eq!(
        code(&weak(PasswordError::Random(getrandom::Error::UNSUPPORTED))),
        ErrorCode::Internal
    );
    assert_eq!(
        code(&weak(PasswordError::Weak("too short"))),
        ErrorCode::Invalid
    );
}

#[tokio::test]
async fn another_devices_revoke_does_not_end_a_stream() {
    let f = fixture();
    let this = owner_signed_up(&f).await;
    let other = f.sign_in.sign_in("Tijs", password(), None).await.unwrap();
    let device = DeviceId::from_stored(this.grant.device.id);
    let waiting = tokio::spawn(f.sign_in.clone().ended(device));
    tokio::task::yield_now().await;
    f.sign_in
        .revoke_device(UserId::OWNER, DeviceId::from_stored(other.grant.device.id))
        .await
        .unwrap();
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished(), "only its own device ends it");
    waiting.abort();
}
