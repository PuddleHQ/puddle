// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn ws(s: &str) -> WorkspaceId {
    WorkspaceId::new(s).unwrap()
}

fn sb(s: &str) -> SandboxName {
    SandboxName::new(s).unwrap()
}

#[test]
fn a_reservation_blocks_everyone_until_attached_or_released() {
    let r = Registry::default();
    r.reserve(&ws("a"), &sb("one")).unwrap();
    let held = r.reserve(&ws("a"), &sb("two")).unwrap_err();
    assert_eq!(held.sandbox, sb("one"));
    assert_eq!(held.kind, HoldKind::Attaching);
    // Even the same sandbox can't create twice at once.
    assert!(r.reserve(&ws("a"), &sb("one")).is_err());
    assert!(r.borrow(&ws("a"), &sb("m")).is_err());
    r.attach(&ws("a"), &sb("one"));
    assert_eq!(r.holder(&ws("a")).unwrap().kind, HoldKind::Attached);
    // The owner may attach again (a recreate under the same name); others may not.
    r.reserve(&ws("a"), &sb("one")).unwrap();
    r.attach(&ws("a"), &sb("one"));
    assert_eq!(
        r.reserve(&ws("a"), &sb("two")).unwrap_err().sandbox,
        sb("one")
    );
    assert_eq!(r.owned_by(&sb("one")), vec![ws("a")]);
    r.release(&ws("a"), &sb("two"));
    assert!(
        r.owner(&ws("a")).is_some(),
        "another sandbox can't release it"
    );
    r.release(&ws("a"), &sb("one"));
    assert_eq!(r.holder(&ws("a")), None);
    r.reserve(&ws("a"), &sb("two")).unwrap();
}

#[test]
fn a_borrower_shows_as_holder_and_blocks_attaches() {
    let r = Registry::default();
    r.attach(&ws("a"), &sb("owner"));
    r.borrow(&ws("a"), &sb("maint")).unwrap();
    assert_eq!(
        r.holder(&ws("a")),
        Some(Holder {
            sandbox: sb("maint"),
            kind: HoldKind::Maintenance
        })
    );
    assert_eq!(r.owner(&ws("a")).unwrap().sandbox, sb("owner"));
    assert_eq!(
        r.reserve(&ws("a"), &sb("owner")).unwrap_err().sandbox,
        sb("maint")
    );
    assert_eq!(
        r.borrow(&ws("a"), &sb("m2")).unwrap_err().sandbox,
        sb("maint")
    );
    r.give_back(&ws("a"));
    assert_eq!(r.holder(&ws("a")).unwrap().sandbox, sb("owner"));
    r.forget(&ws("a"));
    assert_eq!(r.holder(&ws("a")), None);
    // A loan of an unowned workspace leaves nothing behind.
    r.borrow(&ws("b"), &sb("maint")).unwrap();
    r.give_back(&ws("b"));
    assert!(r.lock().is_empty());
}
