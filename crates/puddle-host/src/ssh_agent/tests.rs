// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::BTreeSet;

use puddle_ssh_agent::request;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;

fn row(host: &str, keys: Option<&[&str]>) -> SshAgentHost {
    SshAgentHost {
        host: host.to_owned(),
        keys: keys.map(|k| k.iter().map(|s| (*s).to_owned()).collect()),
        changed_at: 0,
    }
}

#[test]
fn stored_hosts_become_the_proxy_hosts_with_every_key_or_the_listed_ones() {
    let hosts = allowed_hosts(vec![
        row("build.example.com", None),
        row("10.0.0.7", Some(&["SHA256:a", "SHA256:b"])),
        row("Not A Host!", None),
    ]);
    assert_eq!(hosts.len(), 2, "the row that does not parse is left out");
    assert_eq!(
        hosts[&Host::parse_normalised("build.example.com").unwrap()],
        AllowedKeys::All
    );
    assert_eq!(
        hosts[&Host::parse_normalised("10.0.0.7").unwrap()],
        AllowedKeys::Only(BTreeSet::from([
            "SHA256:a".to_owned(),
            "SHA256:b".to_owned()
        ]))
    );
}

/// An agent on a real socket (a named pipe on Windows) that holds one key, named by its comment
/// in what it signs so the test can tell two agents apart.
fn agent(name: &'static str) -> (String, puddle_ipc::IpcRoot) {
    let root = puddle_ipc::IpcRoot::new().unwrap();
    let mut listener = root.listen().unwrap();
    let path = listener.endpoint().path().to_string_lossy().into_owned();
    tokio::spawn(async move {
        // flake-ok: the accept loop of a fake server, not a wait for a condition.
        while let Ok(mut conn) = listener.accept().await {
            tokio::spawn(async move {
                let mut len = [0_u8; 4];
                conn.read_exact(&mut len).await.unwrap();
                let mut asked = vec![0_u8; u32::from_be_bytes(len) as usize];
                conn.read_exact(&mut asked).await.unwrap();
                let answer = match asked.first() {
                    Some(11) => {
                        request::identities(&[&Identity::new(vec![1, 2, 3], String::new())])
                    }
                    _ => request::signature(name.as_bytes()),
                };
                conn.write_all(&answer).await.unwrap();
            });
        }
    });
    (path, root)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_agent_the_settings_name_is_asked_and_a_changed_setting_applies_to_the_next_request() {
    let dir = tempfile::tempdir().unwrap();
    let settings = Arc::new(crate::FileSettings::new(dir.path()));
    let (first, _a) = agent("first");
    let (second, _b) = agent("second");
    settings
        .save_global(serde_json::json!({"ui": {"ssh_agent_pipe": first}}))
        .unwrap();
    let host_agent = SettingsAgent {
        settings: Some(settings.clone()),
        auth_sock: None,
    };
    let host_agent = Arc::new(host_agent);
    let asked = Arc::clone(&host_agent);
    let (keys, signed) = tokio::task::spawn_blocking(move || {
        (asked.list().unwrap(), asked.sign(&[1], b"d", 0).unwrap())
    })
    .await
    .unwrap();
    assert_eq!(keys, vec![Identity::new(vec![1, 2, 3], String::new())]);
    assert_eq!(signed, b"first");

    settings
        .save_global(serde_json::json!({"ui": {"ssh_agent_pipe": second}}))
        .unwrap();
    let signed = tokio::task::spawn_blocking(move || host_agent.sign(&[1], b"d", 0).unwrap())
        .await
        .unwrap();
    assert_eq!(signed, b"second");
}

#[cfg(not(windows))]
#[test]
fn with_no_setting_the_auth_sock_is_asked_and_with_neither_there_is_no_agent() {
    let none = SettingsAgent {
        settings: None,
        auth_sock: None,
    };
    let err = none.list().unwrap_err();
    assert!(
        err.to_string().contains("SSH_AUTH_SOCK is not set"),
        "{err}"
    );
    assert!(none.sign(&[1], b"d", 0).is_err());

    let gone = SettingsAgent {
        settings: None,
        auth_sock: Some("/nonexistent/puddle-test-agent.sock".into()),
    };
    assert!(matches!(gone.list(), Err(AgentError::Unreachable(_))));
}

#[test]
fn a_new_agent_reads_the_auth_sock_of_its_process() {
    let made = SettingsAgent::new(None);
    assert_eq!(made.auth_sock, std::env::var_os("SSH_AUTH_SOCK"));
}

mod gate {
    use puddle_ssh_agent::testing::TestKey;
    use puddle_store::{Limits, ManualClock, SshAgentAnswer, SshAgentAskState};

    use super::*;

    fn ws(name: &str) -> WorkspaceId {
        WorkspaceId::new(name).unwrap()
    }

    /// A store whose workspaces "a" and "b" belong to a user: the writes refuse an unowned one.
    fn store() -> Arc<Store> {
        let store =
            Store::open_in_memory(Arc::new(ManualClock::new(1_000)), Limits::default()).unwrap();
        // The workspaces the tests ask from are the owner's, who answers.
        for name in ["a", "b"] {
            puddle_store::testing::own(&store, &ws(name));
        }
        Arc::new(store)
    }

    /// A gate that waits up to `wait` and looks every millisecond.
    fn gate(store: &Arc<Store>, wait: Duration, refusal_every: Duration) -> Arc<StoreGate> {
        Arc::new(StoreGate::with_timing(
            Arc::clone(store),
            wait,
            Duration::from_millis(1),
            refusal_every,
        ))
    }

    fn answer_when_asked(
        store: &Arc<Store>,
        answer: SshAgentAnswer,
    ) -> std::thread::JoinHandle<()> {
        let store = Arc::clone(store);
        std::thread::spawn(move || {
            let mut waited = 0;
            // flake-ok: a fake user polling for the question the code under test opens.
            while waited < 5_000 {
                if let Some(ask) = store
                    .open_ssh_agent_asks_every_user(&puddle_store::testing::all_users(), None)
                    .unwrap()
                    .first()
                {
                    store
                        .as_user(puddle_store::UserId::OWNER)
                        .answer_ssh_agent_ask(&ask.workspace, ask.id, answer, Actor::Ui)
                        .unwrap();
                    return;
                }
                std::thread::sleep(Duration::from_millis(1));
                waited += 1;
            }
            panic!("no question was put");
        })
    }

    fn audit(store: &Store) -> Vec<String> {
        store
            .audit_lines(0, 100)
            .unwrap()
            .into_iter()
            .map(|(_, line)| line)
            .collect()
    }

    #[test]
    fn a_first_use_waits_for_the_users_answer_then_signs() {
        let store = store();
        let gate = gate(&store, Duration::from_secs(5), Duration::ZERO);
        let key = TestKey::ed25519(2).blob();
        let user = answer_when_asked(&store, SshAgentAnswer::Once);
        assert_eq!(gate.approve(&ws("a"), "h.example.com", &key), Ok(()));
        user.join().unwrap();
        let ask = store
            .ssh_agent_ask(&ws("a"), 1)
            .unwrap()
            .expect("the question is kept for a while");
        assert_eq!(ask.state, SshAgentAskState::AllowedOnce);
        assert_eq!(ask.host, "h.example.com");
        assert_eq!(ask.key_type, "ssh-ed25519");
        assert_eq!(ask.fingerprint, TestKey::ed25519(2).fingerprint());
    }

    #[test]
    fn the_first_host_key_of_a_host_is_kept_and_another_is_refused() {
        let store = store();
        let gate = gate(&store, Duration::from_secs(5), Duration::ZERO);
        assert_eq!(
            gate.pin_host_key(&ws("a"), "h.example.com", "SHA256:first"),
            Err(PinFailure::Unavailable),
            "the agent is not on for the host"
        );
        store
            .set_ssh_agent_host(&ws("a"), "h.example.com", None)
            .unwrap();
        assert_eq!(
            gate.pin_host_key(&ws("a"), "h.example.com", "SHA256:first"),
            Ok(())
        );
        assert_eq!(
            gate.pin_host_key(&ws("a"), "h.example.com", "SHA256:first"),
            Ok(())
        );
        assert_eq!(
            gate.pin_host_key(&ws("a"), "h.example.com", "SHA256:other"),
            Err(PinFailure::Changed("SHA256:first".into()))
        );
    }

    #[test]
    fn a_store_that_cannot_keep_the_host_key_refuses_the_bind() {
        let store = store();
        store
            .set_ssh_agent_host(&ws("a"), "h.example.com", None)
            .unwrap();
        puddle_store::testing::break_database(
            &store,
            "CREATE TRIGGER pin_fails BEFORE UPDATE ON ssh_agent_hosts
             BEGIN SELECT RAISE(ABORT, 'disk full'); END",
        );
        let gate = gate(&store, Duration::from_secs(5), Duration::ZERO);
        assert_eq!(
            gate.pin_host_key(&ws("a"), "h.example.com", "SHA256:first"),
            Err(PinFailure::Unavailable)
        );
    }

    #[test]
    fn a_no_is_a_refusal_and_an_always_stops_the_questions() {
        let store = store();
        let gate = gate(&store, Duration::from_secs(5), Duration::ZERO);
        let key = TestKey::ed25519(2).blob();
        let user = answer_when_asked(&store, SshAgentAnswer::Deny);
        assert_eq!(
            gate.approve(&ws("a"), "h.example.com", &key),
            Err(Disapproval::Denied)
        );
        user.join().unwrap();
        let user = answer_when_asked(&store, SshAgentAnswer::Always);
        assert_eq!(gate.approve(&ws("a"), "h.example.com", &key), Ok(()));
        user.join().unwrap();
        assert_eq!(
            gate.approve(&ws("a"), "h.example.com", &key),
            Ok(()),
            "a standing yes asks nothing"
        );
        assert_eq!(
            store
                .open_ssh_agent_asks_every_user(&puddle_store::testing::all_users(), None)
                .unwrap(),
            []
        );
    }

    #[test]
    fn no_answer_in_time_leaves_the_question_open_for_a_later_try() {
        let store = store();
        let gate = gate(&store, Duration::from_millis(30), Duration::ZERO);
        let key = TestKey::ed25519(2).blob();
        assert_eq!(
            gate.approve(&ws("a"), "h.example.com", &key),
            Err(Disapproval::Unanswered)
        );
        assert_eq!(
            store
                .open_ssh_agent_asks_every_user(&puddle_store::testing::all_users(), None)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            gate.approve(&ws("a"), "h.example.com", &key),
            Err(Disapproval::Unanswered)
        );
        assert_eq!(
            store
                .open_ssh_agent_asks_every_user(&puddle_store::testing::all_users(), None)
                .unwrap()[0]
                .attempts,
            2,
            "the second try joined the question"
        );
    }

    #[test]
    fn a_store_that_cannot_take_the_question_refuses_the_signature() {
        let store = store();
        puddle_store::testing::break_database(
            &store,
            "CREATE TRIGGER asks_insert_fails BEFORE INSERT ON ssh_agent_asks
             BEGIN SELECT RAISE(ABORT, 'disk full'); END",
        );
        let gate = gate(&store, Duration::from_secs(5), Duration::ZERO);
        let key = TestKey::ed25519(2).blob();
        assert_eq!(
            gate.approve(&ws("a"), "h.example.com", &key),
            Err(Disapproval::Unavailable)
        );
        assert_eq!(
            store
                .open_ssh_agent_asks_every_user(&puddle_store::testing::all_users(), None)
                .unwrap(),
            []
        );
    }

    #[test]
    fn a_store_that_cannot_be_read_while_a_login_waits_refuses_the_signature() {
        let store = store();
        let gate = gate(&store, Duration::from_secs(30), Duration::ZERO);
        let key = TestKey::ed25519(2).blob();
        let breaker = {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                // flake-ok: a fake failure, once the question the code under test opens is there.
                while store
                    .open_ssh_agent_asks_every_user(&puddle_store::testing::all_users(), None)
                    .unwrap()
                    .is_empty()
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
                puddle_store::testing::break_database(
                    &store,
                    "ALTER TABLE ssh_agent_asks RENAME TO ssh_agent_asks_gone",
                );
            })
        };
        assert_eq!(
            gate.approve(&ws("a"), "h.example.com", &key),
            Err(Disapproval::Unavailable)
        );
        breaker.join().unwrap();
    }

    #[test]
    fn a_workspace_that_stops_while_a_login_waits_ends_the_wait() {
        let store = store();
        let gate = gate(&store, Duration::from_secs(30), Duration::ZERO);
        let key = TestKey::ed25519(2).blob();
        let stopper = {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                // flake-ok: a fake stop, once the question the code under test opens is there.
                while store
                    .open_ssh_agent_asks_every_user(&puddle_store::testing::all_users(), None)
                    .unwrap()
                    .is_empty()
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
                store.end_ssh_agent_run(Some(&ws("a"))).unwrap();
            })
        };
        assert_eq!(
            gate.approve(&ws("a"), "h.example.com", &key),
            Err(Disapproval::Unanswered)
        );
        stopper.join().unwrap();
    }

    #[test]
    fn something_that_is_not_a_key_is_never_asked_about() {
        let store = store();
        let gate = gate(&store, Duration::from_millis(30), Duration::ZERO);
        assert_eq!(
            gate.approve(&ws("a"), "h.example.com", b"junk"),
            Err(Disapproval::Unavailable)
        );
        assert_eq!(
            store
                .open_ssh_agent_asks_every_user(&puddle_store::testing::all_users(), None)
                .unwrap(),
            []
        );
    }

    #[test]
    fn a_new_gate_clears_what_an_earlier_run_left() {
        let store = store();
        let key = TestKey::ed25519(2).blob();
        // The first gate waits for a real answer; 30 ms ran out under host load before the fake user polled.
        let first = gate(&store, Duration::from_secs(5), Duration::ZERO);
        let user = answer_when_asked(&store, SshAgentAnswer::Run);
        assert_eq!(first.approve(&ws("a"), "h.example.com", &key), Ok(()));
        user.join().unwrap();
        let _second = gate(&store, Duration::from_millis(30), Duration::ZERO);
        let third = gate(&store, Duration::from_millis(30), Duration::ZERO);
        assert_eq!(
            third.approve(&ws("a"), "h.example.com", &key),
            Err(Disapproval::Unanswered),
            "the run grant of the earlier run is gone"
        );
    }

    #[test]
    fn a_gate_over_a_store_that_cannot_clear_the_old_grants_is_made_and_refuses_signatures() {
        let store = store();
        puddle_store::testing::break_database(
            &store,
            "ALTER TABLE ssh_agent_grants RENAME TO ssh_agent_grants_gone",
        );
        let gate = gate(&store, Duration::from_millis(30), Duration::ZERO);
        let key = TestKey::ed25519(2).blob();
        assert_eq!(
            gate.approve(&ws("a"), "h.example.com", &key),
            Err(Disapproval::Unavailable)
        );
    }

    #[test]
    fn a_signature_and_a_refusal_are_written_to_the_audit_log_and_listing_is_not() {
        let store = store();
        let gate = gate(&store, Duration::from_millis(30), Duration::from_secs(60));
        let w = ws("a");
        gate.record(&w, Some("h.example.com"), &Outcome::Listed(2));
        gate.record(
            &w,
            Some("h.example.com"),
            &Outcome::Bound("h.example.com".into()),
        );
        gate.record(&w, None, &Outcome::Refused(Refusal::NotBound));
        assert_eq!(audit(&store), Vec::<String>::new());
        gate.record(
            &w,
            Some("h.example.com"),
            &Outcome::Signed {
                family: "ed25519",
                fingerprint: "SHA256:abc".into(),
            },
        );
        gate.record(
            &w,
            Some("h.example.com"),
            &Outcome::Refused(Refusal::SshSig),
        );
        let lines = audit(&store);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("ssh_agent_signed"), "{}", lines[0]);
        assert!(
            lines[0].contains("signed a login to h.example.com with ed25519 key SHA256:abc"),
            "{}",
            lines[0]
        );
        assert!(lines[1].contains("ssh_agent_refused"), "{}", lines[1]);
        assert!(lines[1].contains("an SSHSIG signature"), "{}", lines[1]);
        assert!(lines[1].contains("\"outcome\":\"failed\""), "{}", lines[1]);
    }

    #[test]
    fn the_same_refusal_is_written_once_a_minute_and_each_signature_is_written() {
        let store = store();
        let w = ws("a");
        let hold = gate(&store, Duration::from_millis(30), Duration::from_secs(60));
        for _ in 0..5 {
            hold.record(&w, Some("h"), &Outcome::Refused(Refusal::SshSig));
        }
        hold.record(&w, Some("h"), &Outcome::Refused(Refusal::Extension));
        hold.record(&ws("b"), Some("h"), &Outcome::Refused(Refusal::SshSig));
        assert_eq!(audit(&store).len(), 3, "one per workspace and cause");
        let none = gate(&store, Duration::from_millis(30), Duration::ZERO);
        let before = audit(&store).len();
        none.record(&w, Some("h"), &Outcome::Refused(Refusal::SshSig));
        none.record(&w, Some("h"), &Outcome::Refused(Refusal::SshSig));
        assert_eq!(audit(&store).len(), before + 2, "with no hold, every one");
    }

    #[test]
    fn many_distinct_refusals_do_not_grow_the_memory_without_bound() {
        let store = store();
        let gate = gate(&store, Duration::from_millis(30), Duration::ZERO);
        for n in 0..(REFUSALS_REMEMBERED * 2) {
            gate.record(
                &ws("a"),
                Some(&format!("h{n}")),
                &Outcome::Refused(Refusal::SshSig),
            );
        }
        let len = gate.refused.lock().unwrap().len();
        assert!(len <= REFUSALS_REMEMBERED, "{len}");
    }

    #[test]
    fn refusals_of_one_kind_differing_only_in_guest_chosen_values_are_written_once() {
        let store = store();
        let gate = gate(&store, Duration::from_millis(30), Duration::from_secs(60));
        for n in 0..300_u32 {
            gate.record(
                &ws("a"),
                Some("h"),
                &Outcome::Refused(Refusal::UnknownFlags(n)),
            );
        }
        assert_eq!(
            audit(&store).len(),
            1,
            "one row per workspace, host and kind"
        );
        assert_eq!(gate.refused.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_flood_of_distinct_refusals_inside_the_window_stops_at_the_cap_and_writes_no_more() {
        let store = store();
        let gate = gate(&store, Duration::from_millis(30), Duration::from_secs(60));
        for n in 0..(REFUSALS_REMEMBERED + 44) {
            gate.record(
                &ws("a"),
                Some(&format!("h{n}")),
                &Outcome::Refused(Refusal::SshSig),
            );
        }
        let len = gate.refused.lock().unwrap().len();
        assert_eq!(len, REFUSALS_REMEMBERED);
        assert_eq!(
            store.audit_lines(0, 1000).unwrap().len(),
            REFUSALS_REMEMBERED
        );
    }
}
