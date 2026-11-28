// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::{Arc, Mutex};

use puddle_types::EventSink;

use super::*;
use crate::clock::ManualClock;
use crate::store::Limits;

#[derive(Default)]
struct Seen(Mutex<Vec<Event>>);

impl EventSink for Seen {
    fn emit(&self, event: Event) {
        self.0.lock().unwrap().push(event);
    }
}

impl Seen {
    fn asks_changed(&self) -> usize {
        let mut events = self.0.lock().unwrap();
        let n = events
            .iter()
            .filter(|e| matches!(e, Event::SshAgentAsksChanged { .. }))
            .count();
        events.clear();
        n
    }
}

fn ws(text: &str) -> WorkspaceId {
    WorkspaceId::new(text).unwrap()
}

fn store() -> (Arc<ManualClock>, Store, Arc<Seen>) {
    let clock = Arc::new(ManualClock::new(1_000));
    let seen = Arc::new(Seen::default());
    let store = Store::open_in_memory(clock.clone(), Limits::default())
        .unwrap()
        .with_events(seen.clone());
    for name in ["a", "b"] {
        store.set_workspace_owner(&ws(name), UserId::OWNER).unwrap();
    }
    (clock, store, seen)
}

const KEY: &str = "SHA256:abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ";

fn open(store: &Store, workspace: &str, host: &str) -> i64 {
    match store
        .start_ssh_agent_ask(&ws(workspace), host, KEY, "ssh-ed25519")
        .unwrap()
    {
        SshAgentAskStart::Open(id) => id,
        SshAgentAskStart::Granted => panic!("a grant answered"),
    }
}

#[test]
fn the_first_request_opens_one_question_that_later_requests_join() {
    let (clock, store, seen) = store();
    let id = open(&store, "a", "git.example.com");
    assert_eq!(seen.asks_changed(), 1);
    clock.advance(500);
    assert_eq!(open(&store, "a", "git.example.com"), id);
    assert_eq!(
        seen.asks_changed(),
        0,
        "a second request changes nothing to show"
    );
    let asks = store.open_ssh_agent_asks(None, None).unwrap();
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0].attempts, 2);
    assert_eq!(asks[0].first_seen, 1_000);
    assert_eq!(asks[0].last_seen, 1_500);
    assert_eq!(asks[0].key_type, "ssh-ed25519");
    assert_ne!(open(&store, "a", "other.example.com"), id);
    assert_ne!(open(&store, "b", "git.example.com"), id);
}

#[test]
fn once_lets_the_waiting_requests_sign_and_leaves_no_grant() {
    let (_, store, seen) = store();
    let id = open(&store, "a", "h.example.com");
    seen.asks_changed();
    let ask = store
        .answer_ssh_agent_ask(&ws("a"), id, SshAgentAnswer::Once, Actor::Ui)
        .unwrap();
    assert_eq!(ask.state, SshAgentAskState::AllowedOnce);
    assert!(ask.state.allows());
    assert_eq!(seen.asks_changed(), 1);
    assert_eq!(store.ssh_agent_ask(id).unwrap().unwrap().state, ask.state);
    assert_eq!(store.open_ssh_agent_asks(None, None).unwrap(), []);
    assert_ne!(
        open(&store, "a", "h.example.com"),
        id,
        "the next login asks again"
    );
}

#[test]
fn deny_lets_nothing_sign() {
    let (_, store, _) = store();
    let id = open(&store, "a", "h.example.com");
    let ask = store
        .answer_ssh_agent_ask(&ws("a"), id, SshAgentAnswer::Deny, Actor::Cli)
        .unwrap();
    assert_eq!(ask.state, SshAgentAskState::Denied);
    assert!(!ask.state.allows());
    assert_ne!(
        open(&store, "a", "h.example.com"),
        id,
        "a denial is no standing no"
    );
}

#[test]
fn run_and_always_grants_answer_later_requests_until_they_end() {
    let (_, store, _) = store();
    let run = open(&store, "a", "run.example.com");
    store
        .answer_ssh_agent_ask(&ws("a"), run, SshAgentAnswer::Run, Actor::Ui)
        .unwrap();
    let always = open(&store, "a", "always.example.com");
    store
        .answer_ssh_agent_ask(&ws("a"), always, SshAgentAnswer::Always, Actor::Ui)
        .unwrap();
    let granted = |host: &str| {
        store
            .start_ssh_agent_ask(&ws("a"), host, KEY, "ssh-ed25519")
            .unwrap()
            == SshAgentAskStart::Granted
    };
    assert!(granted("run.example.com") && granted("always.example.com"));
    assert!(
        !store
            .start_ssh_agent_ask(&ws("b"), "run.example.com", KEY, "ssh-ed25519")
            .unwrap()
            .eq(&SshAgentAskStart::Granted),
        "a grant is for its workspace"
    );
    store.end_ssh_agent_run(Some(&ws("b"))).unwrap();
    assert!(
        granted("run.example.com"),
        "another workspace's stop leaves it"
    );
    store.end_ssh_agent_run(Some(&ws("a"))).unwrap();
    assert!(granted("always.example.com"));
    assert!(!granted("run.example.com"), "the workspace stopped");
}

#[test]
fn clearing_every_run_grant_leaves_the_always_ones() {
    let (_, store, _) = store();
    for (host, answer) in [
        ("r.example.com", SshAgentAnswer::Run),
        ("a.example.com", SshAgentAnswer::Always),
    ] {
        let id = open(&store, "a", host);
        store
            .answer_ssh_agent_ask(&ws("a"), id, answer, Actor::Ui)
            .unwrap();
    }
    store.end_ssh_agent_run(None).unwrap();
    let start = |host: &str| {
        store
            .start_ssh_agent_ask(&ws("a"), host, KEY, "ssh-ed25519")
            .unwrap()
    };
    assert_eq!(start("a.example.com"), SshAgentAskStart::Granted);
    assert_ne!(start("r.example.com"), SshAgentAskStart::Granted);
}

#[test]
fn an_answer_is_final_and_belongs_to_one_workspace() {
    let (_, store, _) = store();
    let id = open(&store, "a", "h.example.com");
    let wrong = store.answer_ssh_agent_ask(&ws("b"), id, SshAgentAnswer::Once, Actor::Ui);
    assert!(matches!(wrong, Err(StoreError::UnknownSshAgentAsk(n)) if n == id));
    let missing = store.answer_ssh_agent_ask(&ws("a"), id + 99, SshAgentAnswer::Once, Actor::Ui);
    assert!(matches!(missing, Err(StoreError::UnknownSshAgentAsk(_))));
    store
        .answer_ssh_agent_ask(&ws("a"), id, SshAgentAnswer::Deny, Actor::Ui)
        .unwrap();
    let again = store.answer_ssh_agent_ask(&ws("a"), id, SshAgentAnswer::Always, Actor::Ui);
    assert!(
        matches!(
            again,
            Err(StoreError::SshAgentAskNotOpen {
                state: "denied",
                ..
            })
        ),
        "{again:?}"
    );
}

#[test]
fn an_answer_is_written_to_the_audit_log_with_who_gave_it() {
    let (_, store, _) = store();
    let id = open(&store, "a", "h.example.com");
    store
        .answer_ssh_agent_ask(&ws("a"), id, SshAgentAnswer::Always, Actor::Cli)
        .unwrap();
    let line = store
        .audit_lines(0, 10)
        .unwrap()
        .into_iter()
        .map(|(_, line)| line)
        .find(|l| l.contains("ssh_agent_answered"))
        .expect("an audit record");
    assert!(line.contains("\"actor\":\"cli\""), "{line}");
    assert!(
        line.contains("allowed always for ssh-ed25519 key SHA256:"),
        "{line}"
    );
    assert!(line.contains("h.example.com"), "{line}");
}

#[test]
fn turning_the_agent_off_for_a_host_drops_its_grants_and_open_questions() {
    let (_, store, seen) = store();
    store
        .set_ssh_agent_host(&ws("a"), "h.example.com", None)
        .unwrap();
    let granted = open(&store, "a", "h.example.com");
    store
        .answer_ssh_agent_ask(&ws("a"), granted, SshAgentAnswer::Always, Actor::Ui)
        .unwrap();
    let waiting = store
        .start_ssh_agent_ask(&ws("a"), "h.example.com", "SHA256:other", "ssh-rsa")
        .unwrap();
    assert!(matches!(waiting, SshAgentAskStart::Open(_)));
    seen.asks_changed();
    assert!(
        store
            .remove_ssh_agent_host(&ws("a"), "h.example.com")
            .unwrap()
    );
    assert_eq!(seen.asks_changed(), 1);
    assert_eq!(store.open_ssh_agent_asks(None, None).unwrap(), []);
    assert_ne!(
        store
            .start_ssh_agent_ask(&ws("a"), "h.example.com", KEY, "ssh-ed25519")
            .unwrap(),
        SshAgentAskStart::Granted
    );
}

#[test]
fn deleting_a_workspace_removes_its_questions_and_grants() {
    let (_, store, _) = store();
    let id = open(&store, "a", "h.example.com");
    store
        .answer_ssh_agent_ask(&ws("a"), id, SshAgentAnswer::Always, Actor::Ui)
        .unwrap();
    open(&store, "a", "other.example.com");
    store.delete_workspace(&ws("a")).unwrap();
    assert_eq!(store.open_ssh_agent_asks(None, None).unwrap(), []);
    assert_ne!(open(&store, "a", "h.example.com"), 0);
}

#[test]
fn one_workspace_may_have_only_so_many_questions_open() {
    let (_, store, _) = store();
    for n in 0..MAX_OPEN_SSH_AGENT_ASKS {
        open(&store, "a", &format!("h{n}.example.com"));
    }
    let err = store.start_ssh_agent_ask(&ws("a"), "more.example.com", KEY, "ssh-ed25519");
    assert!(
        matches!(err, Err(StoreError::TooManySshAgentAsks(_))),
        "{err:?}"
    );
    open(&store, "b", "more.example.com");
    open(&store, "a", "h0.example.com");
}

#[test]
fn decided_questions_are_pruned_after_an_hour() {
    let (clock, store, _) = store();
    let id = open(&store, "a", "h.example.com");
    store
        .answer_ssh_agent_ask(&ws("a"), id, SshAgentAnswer::Once, Actor::Ui)
        .unwrap();
    clock.advance(DECIDED_KEPT_MS - 1);
    open(&store, "a", "x.example.com");
    assert!(store.ssh_agent_ask(id).unwrap().is_some());
    clock.advance(2);
    open(&store, "a", "y.example.com");
    assert!(store.ssh_agent_ask(id).unwrap().is_none());
}

#[test]
fn a_user_sees_only_the_questions_of_their_own_workspaces() {
    let (_, store, _) = store();
    let ann = store
        .admin_add_user(UserId::OWNER, &crate::UserName::new("ann").unwrap())
        .unwrap()
        .id;
    store.set_workspace_owner(&ws("c"), ann).unwrap();
    let mine = open(&store, "a", "h.example.com");
    let hers = open(&store, "c", "h.example.com");
    let ids = |user: UserId, workspace: Option<&WorkspaceId>| -> Vec<i64> {
        let mut ids: Vec<i64> = store
            .open_ssh_agent_asks(Some(user), workspace)
            .unwrap()
            .iter()
            .map(|ask| ask.id)
            .collect();
        ids.sort_unstable();
        ids
    };
    assert_eq!(ids(UserId::OWNER, None), [mine]);
    assert_eq!(ids(ann, None), [hers]);
    assert_eq!(ids(ann, Some(&ws("a"))), Vec::<i64>::new());
    assert_eq!(ids(UserId::from_stored(999), None), Vec::<i64>::new());
    assert_eq!(store.open_ssh_agent_asks(None, None).unwrap().len(), 2);
}

#[test]
fn ending_a_run_drops_its_open_questions_and_tells_the_ui_once_per_workspace() {
    let (_, store, seen) = store();
    open(&store, "a", "h.example.com");
    open(&store, "a", "i.example.com");
    open(&store, "b", "h.example.com");
    seen.asks_changed();
    store.end_ssh_agent_run(Some(&ws("a"))).unwrap();
    assert_eq!(seen.asks_changed(), 1);
    let left = store.open_ssh_agent_asks(None, None).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].workspace, ws("b"));
    store.end_ssh_agent_run(None).unwrap();
    assert_eq!(seen.asks_changed(), 1);
    assert_eq!(store.open_ssh_agent_asks(None, None).unwrap(), []);
    store.end_ssh_agent_run(None).unwrap();
    assert_eq!(seen.asks_changed(), 0, "nothing was waiting");
}

#[test]
fn every_state_round_trips_through_its_text_and_unknown_text_is_none() {
    for state in [
        SshAgentAskState::Requested,
        SshAgentAskState::AllowedOnce,
        SshAgentAskState::AllowedRun,
        SshAgentAskState::AllowedAlways,
        SshAgentAskState::Denied,
    ] {
        assert_eq!(SshAgentAskState::parse(state.as_str()), Some(state));
    }
    assert_eq!(SshAgentAskState::parse("maybe"), None);
}

#[test]
fn an_answer_whose_audit_record_cannot_be_written_is_not_given() {
    let (_, store, _) = store();
    let id = open(&store, "a", "h.example.com");
    store.fail_audit_writes("workspace_op");

    let failed = store.answer_ssh_agent_ask(&ws("a"), id, SshAgentAnswer::Always, Actor::Ui);

    assert!(matches!(failed, Err(StoreError::Database(_))), "{failed:?}");
    store.audit_writes_work_again();
    let ask = store.ssh_agent_ask(id).unwrap().unwrap();
    assert_eq!(
        ask.state,
        SshAgentAskState::Requested,
        "the transaction rolled back"
    );
    assert!(
        store
            .audit_lines(0, 10)
            .unwrap()
            .iter()
            .all(|(_, l)| !l.contains("ssh_agent_answered"))
    );
}

#[test]
fn a_request_that_cannot_be_counted_on_its_open_question_fails_and_changes_nothing() {
    let (_, store, _) = store();
    let id = open(&store, "a", "h.example.com");
    store.break_database(
        "CREATE TRIGGER asks_update_fails BEFORE UPDATE ON ssh_agent_asks
         BEGIN SELECT RAISE(ABORT, 'disk full'); END",
    );

    let failed = store.start_ssh_agent_ask(&ws("a"), "h.example.com", KEY, "ssh-ed25519");

    assert!(matches!(failed, Err(StoreError::Database(_))), "{failed:?}");
    store.break_database("DROP TRIGGER asks_update_fails");
    let joined = store
        .start_ssh_agent_ask(&ws("a"), "h.example.com", KEY, "ssh-ed25519")
        .unwrap();
    assert!(matches!(joined, SshAgentAskStart::Open(n) if n == id));
}

#[test]
fn a_store_that_cannot_count_the_open_questions_opens_none() {
    let (_, store, _) = store();
    store.refuse_sql_function("count");

    let failed = store.start_ssh_agent_ask(&ws("a"), "h.example.com", KEY, "ssh-ed25519");

    assert!(matches!(failed, Err(StoreError::Database(_))), "{failed:?}");
}

#[test]
fn an_answer_returns_the_question_as_stored_with_the_time_it_was_decided() {
    let (clock, store, _) = store();
    let id = open(&store, "a", "h.example.com");
    assert_eq!(store.ssh_agent_ask(id).unwrap().unwrap().decided_at, None);
    clock.advance(5_000);

    let ask = store
        .answer_ssh_agent_ask(&ws("a"), id, SshAgentAnswer::Once, Actor::Ui)
        .unwrap();

    assert!(ask.decided_at.is_some());
    assert_eq!(store.ssh_agent_ask(id).unwrap().unwrap(), ask);
}

#[test]
fn breaking_the_database_through_the_testing_helper_makes_writes_fail() {
    let (_, store, _) = store();
    crate::testing::break_database(
        &store,
        "CREATE TRIGGER asks_insert_fails BEFORE INSERT ON ssh_agent_asks
         BEGIN SELECT RAISE(ABORT, 'disk full'); END",
    );

    let failed = store.start_ssh_agent_ask(&ws("a"), "h.example.com", KEY, "ssh-ed25519");

    assert!(matches!(failed, Err(StoreError::Database(_))), "{failed:?}");
}

#[test]
#[should_panic(expected = "breaking the database")]
fn breaking_the_database_with_refused_sql_panics() {
    let (_, store, _) = store();
    crate::testing::break_database(&store, "DROP TABLE no_such_table");
}
