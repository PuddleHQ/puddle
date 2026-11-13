// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn id(name: &str) -> WorkspaceId {
    WorkspaceId::new(name).unwrap()
}

fn size() -> TerminalSize {
    TerminalSize::new(80, 24).unwrap()
}

#[test]
fn the_limits_are_the_ones_the_screen_design_sets() {
    assert_eq!(WINDOW_BYTES, 1_048_576);
    assert_eq!(MAX_MESSAGE_BYTES, 65_536);
}

#[test]
fn sizes_are_held_to_their_bounds() {
    assert!(TerminalSize::new(2, 1).is_some());
    assert!(TerminalSize::new(500, 300).is_some());
    for (cols, rows) in [(1, 24), (501, 24), (80, 0), (80, 301), (70_000, 24)] {
        assert_eq!(TerminalSize::new(cols, rows), None, "{cols}x{rows}");
    }
}

#[tokio::test]
async fn a_build_without_terminals_says_so_for_a_check_and_an_open() {
    let none = NoTerminals;
    assert!(matches!(
        none.check(&id("a")).await,
        Err(TerminalError::Unavailable(_))
    ));
    assert!(matches!(
        none.open(&id("a"), size()).await,
        Err(TerminalError::Unavailable(_))
    ));
}

#[test]
fn a_ticket_already_used_keeps_its_place_while_new_ones_are_made() {
    let registry = Registry::default();
    let a = id("a");
    let first = registry.create(&a, size(), "t1".into()).unwrap();
    registry.claim(&a, first, b"t1").unwrap();
    registry.create(&a, size(), "t2".into()).unwrap();
    assert_eq!(registry.claim(&a, first, b"t1"), None);
}

#[tokio::test(start_paused = true)]
async fn a_ticket_works_once_for_its_own_terminal_within_thirty_seconds() {
    let registry = Registry::default();
    let (a, b) = (id("a"), id("b"));
    let first = registry.create(&a, size(), "t1".into()).unwrap();
    let second = registry.create(&a, size(), "t2".into()).unwrap();
    assert_eq!(registry.claim(&b, first, b"t1"), None, "another workspace");
    assert_eq!(
        registry.claim(&a, first, b"t2"),
        None,
        "another terminal's ticket"
    );
    assert_eq!(
        registry.claim(&a, first, b"t1"),
        Some(size()),
        "a wrong guess spares it"
    );
    assert_eq!(registry.claim(&a, first, b"t1"), None, "once");
    tokio::time::advance(TICKET_LIFETIME).await;
    assert_eq!(registry.claim(&a, second, b"t2"), None, "late");
}

#[tokio::test(start_paused = true)]
async fn the_limits_count_open_terminals_and_unused_tickets_and_free_with_the_socket() {
    let registry = Registry::default();
    let a = id("a");
    let ids: Vec<u64> = (0..MAX_PER_WORKSPACE)
        .map(|_| registry.create(&a, size(), "t".into()).unwrap())
        .collect();
    assert_eq!(
        registry.create(&a, size(), "t".into()),
        Err(Limit::Workspace)
    );
    registry.release(ids[0]);
    assert!(registry.create(&a, size(), "t".into()).is_ok());
    let others: Vec<WorkspaceId> = (0..4).map(|n| id(&format!("w{n}"))).collect();
    for other in &others {
        for _ in 0..MAX_PER_WORKSPACE {
            if registry.create(other, size(), "t".into()).is_err() {
                break;
            }
        }
    }
    assert_eq!(
        registry.create(&id("z"), size(), "t".into()),
        Err(Limit::All)
    );
    tokio::time::advance(TICKET_LIFETIME).await;
    assert!(
        registry.create(&id("z"), size(), "t".into()).is_ok(),
        "unused tickets run out"
    );
}

#[test]
fn a_terminal_made_for_a_signed_in_browser_remembers_which() {
    let registry = Registry::default();
    let a = id("a");
    let made = registry.create(&a, size(), "t".into()).unwrap();
    assert_eq!(
        registry.device_of(made),
        None,
        "made with the install token"
    );
    registry.bind(made, puddle_store::DeviceId::from_stored(7));
    assert_eq!(
        registry.device_of(made),
        Some(puddle_store::DeviceId::from_stored(7))
    );
    // Binding a terminal that is gone, or asking about one, changes nothing.
    registry.release(made);
    registry.bind(made, puddle_store::DeviceId::from_stored(8));
    assert_eq!(registry.device_of(made), None);
}
