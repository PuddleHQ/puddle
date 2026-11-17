// SPDX-License-Identifier: AGPL-3.0-or-later
use std::cell::RefCell;
use std::collections::BTreeMap;

use super::*;
use crate::testing::{TestKey, bind, framed, login, sign_request};

/// A sign request the fake agent was asked: key, data, flags.
type Asked = (Vec<u8>, Vec<u8>, u32);

/// A workspace with spliced sessions, the user's switches and an agent, all in memory.
#[derive(Default)]
struct Fake {
    seen: Vec<(KeyData, Vec<u8>, String)>,
    allowed: BTreeMap<String, AllowedKeys>,
    keys: Vec<Identity>,
    agent_down: bool,
    /// The answer to the first-use question: `None` is a yes.
    approval: Option<Disapproval>,
    approved: RefCell<Vec<(String, Vec<u8>)>>,
    signed: RefCell<Vec<Asked>>,
    /// The host key kept per host.
    pinned: RefCell<BTreeMap<String, String>>,
    pin_down: bool,
}

impl Context for Fake {
    fn seen(&self, host_key: &KeyData, signature: &[u8]) -> Option<String> {
        self.seen
            .iter()
            .find(|(k, s, _)| k == host_key && s == signature)
            .map(|(_, _, h)| h.clone())
    }
    fn allowed(&self, host: &str) -> Option<AllowedKeys> {
        self.allowed.get(host).cloned()
    }
    fn list(&self) -> Result<Vec<Identity>, AgentError> {
        if self.agent_down {
            return Err(AgentError::Refused);
        }
        Ok(self.keys.clone())
    }
    fn pin(&self, host: &str, host_key: &str) -> Result<(), PinFailure> {
        if self.pin_down {
            return Err(PinFailure::Unavailable);
        }
        let mut pinned = self.pinned.borrow_mut();
        let kept = pinned
            .entry(host.to_owned())
            .or_insert_with(|| host_key.to_owned());
        if kept == host_key {
            Ok(())
        } else {
            Err(PinFailure::Changed(kept.clone()))
        }
    }
    fn approve(&self, host: &str, key: &[u8]) -> Result<(), Disapproval> {
        self.approved
            .borrow_mut()
            .push((host.to_owned(), key.to_vec()));
        self.approval.map_or(Ok(()), Err)
    }
    fn sign(&self, key: &[u8], data: &[u8], flags: u32) -> Result<Vec<u8>, AgentError> {
        if self.agent_down {
            return Err(AgentError::Refused);
        }
        self.signed
            .borrow_mut()
            .push((key.to_vec(), data.to_vec(), flags));
        Ok(b"SIGNATURE".to_vec())
    }
}

const SESSION: [u8; 32] = [0x5e; 32];

struct World {
    host: TestKey,
    user: TestKey,
    other_user: TestKey,
    fake: Fake,
}

/// `build.example` is spliced with its key exchange seen, the agent is on for it with every key,
/// and the host's agent holds two keys.
fn world() -> World {
    let host = TestKey::ed25519(1);
    let user = TestKey::ed25519(2);
    let other_user = TestKey::ed25519(3);
    let mut fake = Fake::default();
    fake.seen
        .push((host.key_data(), host.sign(&SESSION), "build.example".into()));
    fake.allowed
        .insert("build.example".into(), AllowedKeys::All);
    fake.keys = vec![
        Identity::new(user.blob(), "me@desk".into()),
        Identity::new(other_user.blob(), "work".into()),
    ];
    World {
        host,
        user,
        other_user,
        fake,
    }
}

fn the_bind(w: &World) -> Vec<u8> {
    bind(&w.host.blob(), &SESSION, &w.host.sign(&SESSION), false)
}

fn bound(w: &World) -> Filter {
    let mut filter = Filter::new();
    let (answer, outcome) = filter.answer(&the_bind(w), &w.fake);
    assert_eq!(outcome, Outcome::Bound("build.example".into()));
    assert_eq!(answer, request::success());
    filter
}

fn list_answer(keys: &[&TestKey]) -> Vec<u8> {
    let ids: Vec<Identity> = keys
        .iter()
        .map(|k| Identity::new(k.blob(), String::new()))
        .collect();
    request::identities(&ids.iter().collect::<Vec<_>>())
}

fn signed(key: &TestKey) -> Outcome {
    Outcome::Signed {
        family: "ed25519",
        fingerprint: key.fingerprint(),
    }
}

fn refused(why: Refusal) -> (Vec<u8>, Outcome) {
    (request::failure(), Outcome::Refused(why))
}

#[test]
fn a_bound_session_lists_the_allowed_keys_and_signs_its_own_login() {
    let w = world();
    let mut filter = bound(&w);
    assert_eq!(filter.host(), Some("build.example"));
    assert_eq!(
        filter.answer(&[11], &w.fake),
        (list_answer(&[&w.user, &w.other_user]), Outcome::Listed(2))
    );
    let data = login(&SESSION, &w.user.blob(), None);
    assert_eq!(
        filter.answer(&sign_request(&w.user.blob(), &data, 4), &w.fake),
        (request::signature(b"SIGNATURE"), signed(&w.user))
    );
    assert_eq!(*w.fake.signed.borrow(), vec![(w.user.blob(), data, 4)]);
    let hostbound = login(&SESSION, &w.user.blob(), Some(&w.host.blob()));
    let (_, outcome) = filter.answer(&sign_request(&w.user.blob(), &hostbound, 0), &w.fake);
    assert_eq!(outcome, signed(&w.user));
}

#[test]
fn only_the_keys_allowed_for_the_host_are_listed_and_used() {
    let mut w = world();
    w.fake.allowed.insert(
        "build.example".into(),
        AllowedKeys::Only([w.other_user.fingerprint()].into()),
    );
    let mut filter = bound(&w);
    assert_eq!(
        filter.answer(&[11], &w.fake),
        (list_answer(&[&w.other_user]), Outcome::Listed(1))
    );
    let data = login(&SESSION, &w.user.blob(), None);
    assert_eq!(
        filter.answer(&sign_request(&w.user.blob(), &data, 0), &w.fake),
        refused(Refusal::KeyNotAllowed)
    );
    let data = login(&SESSION, &w.other_user.blob(), None);
    assert_eq!(
        filter
            .answer(&sign_request(&w.other_user.blob(), &data, 0), &w.fake)
            .1,
        signed(&w.other_user)
    );
    let not_a_key = login(&SESSION, b"junk", None);
    assert_eq!(
        filter.answer(&sign_request(b"junk", &not_a_key, 0), &w.fake),
        refused(Refusal::KeyNotAllowed)
    );
}

#[test]
fn before_a_bind_the_list_is_empty_and_nothing_is_signed() {
    let w = world();
    let mut filter = Filter::new();
    assert_eq!(filter.host(), None);
    assert_eq!(
        filter.answer(&[11], &w.fake),
        (list_answer(&[]), Outcome::Refused(Refusal::NotBound))
    );
    let data = login(&SESSION, &w.user.blob(), None);
    assert_eq!(
        filter.answer(&sign_request(&w.user.blob(), &data, 0), &w.fake),
        refused(Refusal::NotBound)
    );
    assert!(w.fake.signed.borrow().is_empty());
}

#[test]
fn a_bind_for_a_session_no_spliced_connection_showed_is_refused() {
    let w = world();
    let mut filter = Filter::new();
    // The same host key, but another session (one the guest opened through a jump host).
    let elsewhere = [0x77; 32];
    let message = bind(&w.host.blob(), &elsewhere, &w.host.sign(&elsewhere), false);
    assert_eq!(
        filter.answer(&message, &w.fake),
        refused(Refusal::UnseenSession)
    );
    assert_eq!(
        filter.answer(&[11], &w.fake).1,
        Outcome::Refused(Refusal::NotBound)
    );
}

#[test]
fn a_bind_whose_signature_does_not_check_over_its_session_is_refused() {
    let w = world();
    let mut filter = Filter::new();
    // The seen signature with a session id of the guest's choosing.
    let message = bind(&w.host.blob(), &[0x99; 32], &w.host.sign(&SESSION), false);
    assert_eq!(
        filter.answer(&message, &w.fake),
        refused(Refusal::BadBindSignature)
    );
    assert_eq!(filter.host(), None);
}

#[test]
fn a_bind_to_a_host_the_agent_is_off_for_is_refused() {
    let mut w = world();
    w.fake.allowed.clear();
    let mut filter = Filter::new();
    assert_eq!(
        filter.answer(&the_bind(&w), &w.fake),
        refused(Refusal::AgentOff("build.example".into()))
    );
}

#[test]
fn turning_the_agent_off_acts_on_a_connection_already_bound() {
    let mut w = world();
    let mut filter = bound(&w);
    w.fake.allowed.clear();
    let off = Outcome::Refused(Refusal::AgentOff("build.example".into()));
    assert_eq!(
        filter.answer(&[11], &w.fake),
        (list_answer(&[]), off.clone())
    );
    let data = login(&SESSION, &w.user.blob(), None);
    assert_eq!(
        filter.answer(&sign_request(&w.user.blob(), &data, 0), &w.fake),
        (request::failure(), off)
    );
    assert!(
        w.fake.signed.borrow().is_empty(),
        "the host's agent was not asked"
    );
}

#[test]
fn a_bind_with_a_git_hosts_key_is_refused_even_when_seen_and_on() {
    use ssh_key::PublicKey;
    let mut w = world();
    let github = PublicKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl",
    )
    .unwrap();
    // A signature this test cannot make; the deny list decides before any lookup or check.
    w.fake.seen.push((
        github.key_data().clone(),
        b"sig".to_vec(),
        "jump.example".into(),
    ));
    w.fake
        .allowed
        .insert("jump.example".into(), AllowedKeys::All);
    let message = bind(&github.to_bytes().unwrap(), &SESSION, b"sig", false);
    assert_eq!(
        Filter::new().answer(&message, &w.fake),
        refused(Refusal::GitHostKey)
    );
}

#[test]
fn a_bind_for_forwarding_a_second_bind_and_a_bad_bind_are_refused() {
    let w = world();
    let forwarding = bind(&w.host.blob(), &SESSION, &w.host.sign(&SESSION), true);
    assert_eq!(
        Filter::new().answer(&forwarding, &w.fake),
        refused(Refusal::Forwarding)
    );
    let mut filter = bound(&w);
    assert_eq!(
        filter.answer(&the_bind(&w), &w.fake),
        refused(Refusal::SecondBind)
    );
    assert_eq!(filter.host(), None, "a second bind unbinds the connection");
    let data = login(&SESSION, &w.user.blob(), None);
    assert_eq!(
        filter.answer(&sign_request(&w.user.blob(), &data, 0), &w.fake),
        refused(Refusal::NotBound)
    );
    let mut bad = the_bind(&w);
    bad.pop();
    assert_eq!(
        Filter::new().answer(&bad, &w.fake),
        refused(Refusal::Malformed)
    );
}

#[test]
fn a_login_for_another_session_or_server_is_refused() {
    let w = world();
    let mut filter = bound(&w);
    let other_session = login(&[0x11; 32], &w.user.blob(), None);
    assert_eq!(
        filter.answer(&sign_request(&w.user.blob(), &other_session, 0), &w.fake),
        refused(Refusal::OtherSession)
    );
    let other_server = login(&SESSION, &w.user.blob(), Some(&w.other_user.blob()));
    assert_eq!(
        filter.answer(&sign_request(&w.user.blob(), &other_server, 0), &w.fake),
        refused(Refusal::OtherSession)
    );
    let junk_server = login(&SESSION, &w.user.blob(), Some(b"junk"));
    assert_eq!(
        filter.answer(&sign_request(&w.user.blob(), &junk_server, 0), &w.fake),
        refused(Refusal::OtherSession)
    );
    // A login naming one key, signed with another.
    let mismatch = login(&SESSION, &w.other_user.blob(), None);
    assert_eq!(
        filter.answer(&sign_request(&w.user.blob(), &mismatch, 0), &w.fake),
        refused(Refusal::KeyNotAllowed)
    );
    assert!(w.fake.signed.borrow().is_empty());
}

#[test]
fn commit_signing_and_any_other_data_are_refused() {
    let w = world();
    let mut filter = bound(&w);
    let sshsig = b"SSHSIG\0\0\0\x03git".to_vec();
    assert_eq!(
        filter.answer(&sign_request(&w.user.blob(), &sshsig, 0), &w.fake),
        refused(Refusal::SshSig)
    );
    assert_eq!(
        filter.answer(&sign_request(&w.user.blob(), b"anything", 0), &w.fake),
        refused(Refusal::NotLogin)
    );
    let data = login(&SESSION, &w.user.blob(), None);
    assert_eq!(
        filter.answer(&sign_request(&w.user.blob(), &data, 1), &w.fake),
        refused(Refusal::UnknownFlags(1))
    );
    assert_eq!(
        filter.answer(&sign_request(&w.user.blob(), &data, 8 | 4), &w.fake),
        refused(Refusal::UnknownFlags(12))
    );
    assert!(w.fake.signed.borrow().is_empty());
    assert_eq!(
        filter
            .answer(&sign_request(&w.user.blob(), &data, 2 | 4), &w.fake)
            .1,
        signed(&w.user)
    );
}

#[test]
fn adding_removing_locking_and_other_extensions_are_refused() {
    let w = world();
    let mut filter = bound(&w);
    for kind in [17, 18, 19, 20, 21, 22, 23, 25, 26] {
        assert_eq!(
            filter.answer(&[kind, 0, 0, 0, 0], &w.fake),
            refused(Refusal::Unsupported(kind))
        );
    }
    let mut query = vec![27];
    query.extend(crate::testing::string(b"query"));
    assert_eq!(filter.answer(&query, &w.fake), refused(Refusal::Extension));
    assert_eq!(filter.answer(&[], &w.fake), refused(Refusal::Malformed));
    assert_eq!(filter.host(), Some("build.example"), "still bound");
}

#[test]
fn a_failing_host_agent_is_a_failure_answer() {
    let mut w = world();
    let mut filter = bound(&w);
    w.fake.agent_down = true;
    assert_eq!(
        filter.answer(&[11], &w.fake),
        refused(Refusal::Agent(
            "the agent refused the request (locked?)".into()
        ))
    );
    let data = login(&SESSION, &w.user.blob(), None);
    assert_eq!(
        filter.answer(&sign_request(&w.user.blob(), &data, 0), &w.fake),
        refused(Refusal::Agent(
            "the agent refused the request (locked?)".into()
        ))
    );
}

#[test]
fn each_refusal_says_why_in_words() {
    let cases = [
        (Refusal::Malformed, "not agent protocol"),
        (Refusal::NotBound, "no session bind on this connection"),
        (Refusal::SecondBind, "a second session bind"),
        (Refusal::Forwarding, "a bind for agent forwarding"),
        (Refusal::GitHostKey, "a Git host's key"),
        (
            Refusal::UnseenSession,
            "a session no spliced connection showed",
        ),
        (Refusal::AgentOff("h".into()), "the agent is off for h"),
        (
            Refusal::BadBindSignature,
            "the bind's signature does not check",
        ),
        (
            Refusal::HostKeyChanged {
                kept: "SHA256:k".into(),
                shown: "SHA256:s".into(),
            },
            "host key SHA256:s is not the host's key SHA256:k; turn the agent off and on again for the host to accept a new key",
        ),
        (
            Refusal::HostKeyUnchecked,
            "the host's key could not be checked",
        ),
        (Refusal::SshSig, "an SSHSIG signature"),
        (Refusal::NotLogin, "data that is not a login"),
        (Refusal::OtherSession, "a login for another session"),
        (Refusal::KeyNotAllowed, "a key not allowed for the host"),
        (Refusal::UnknownFlags(9), "sign flags 0x9"),
        (
            Refusal::Extension,
            "an extension other than the session bind",
        ),
        (Refusal::Unsupported(17), "request type 17"),
        (Refusal::Agent("x".into()), "the host's agent: x"),
    ];
    for (refusal, words) in cases {
        assert_eq!(refusal.to_string(), words);
    }
}

#[test]
fn a_fingerprint_is_the_sha256_form_and_none_for_junk() {
    let key = TestKey::ed25519(2);
    assert_eq!(fingerprint(&key.blob()), Some(key.fingerprint()));
    assert_eq!(fingerprint(b"junk"), None);
    assert!(key.fingerprint().starts_with("SHA256:"));
}

#[test]
fn the_answers_are_framed() {
    // A sanity check that the helpers agree with the answers the filter writes.
    assert_eq!(request::failure(), framed(&[5]));
}

#[test]
fn a_signature_waits_for_the_first_use_answer_and_asks_for_the_bound_host_and_key() {
    let w = world();
    let mut filter = bound(&w);
    let data = login(&SESSION, &w.user.blob(), None);
    filter.answer(&sign_request(&w.user.blob(), &data, 0), &w.fake);
    assert_eq!(
        *w.fake.approved.borrow(),
        vec![("build.example".to_owned(), w.user.blob())]
    );
}

#[test]
fn a_first_use_that_is_not_approved_signs_nothing_and_says_why() {
    for why in [
        Disapproval::Denied,
        Disapproval::Unanswered,
        Disapproval::Unavailable,
    ] {
        let mut w = world();
        w.fake.approval = Some(why);
        let mut filter = bound(&w);
        let data = login(&SESSION, &w.user.blob(), None);
        assert_eq!(
            filter.answer(&sign_request(&w.user.blob(), &data, 0), &w.fake),
            refused(Refusal::Disapproved(why))
        );
        assert!(w.fake.signed.borrow().is_empty(), "{why:?}");
        assert_eq!(
            filter.answer(&[11], &w.fake).1,
            Outcome::Listed(2),
            "listing keys asks nothing"
        );
    }
}

#[test]
fn no_question_is_put_for_a_request_that_is_refused_anyway() {
    let w = world();
    let mut filter = bound(&w);
    let other_session = login(&[0x77; 32], &w.user.blob(), None);
    filter.answer(&sign_request(&w.user.blob(), &other_session, 0), &w.fake);
    filter.answer(&sign_request(&w.user.blob(), b"not a login", 0), &w.fake);
    filter.answer(
        &sign_request(&w.user.blob(), &login(&SESSION, &w.user.blob(), None), 1),
        &w.fake,
    );
    assert_eq!(*w.fake.approved.borrow(), vec![]);
}

#[test]
fn a_key_blob_names_its_type_and_a_junk_blob_has_none() {
    let w = world();
    assert_eq!(key_type(&w.user.blob()).as_deref(), Some("ssh-ed25519"));
    assert_eq!(key_type(b"junk"), None);
}

#[test]
fn a_first_use_that_is_not_approved_says_why_in_words() {
    for (why, words) in [
        (Disapproval::Denied, "the user said no"),
        (Disapproval::Unanswered, "the user did not answer in time"),
        (
            Disapproval::Unavailable,
            "the question could not be put to the user",
        ),
    ] {
        assert_eq!(why.to_string(), words);
        assert_eq!(
            Refusal::Disapproved(why).to_string(),
            format!("first use not approved: {words}")
        );
    }
}

#[test]
fn a_second_host_key_at_the_same_name_is_refused_and_nothing_is_signed() {
    // A server at build.example relays another server's key exchange: the proxy sees that
    // server's host key under the name build.example.
    let mut w = world();
    let elsewhere = TestKey::ed25519(9);
    let other_session = [0x77; 32];
    let _first = bound(&w);
    w.fake.seen.push((
        elsewhere.key_data(),
        elsewhere.sign(&other_session),
        "build.example".into(),
    ));
    let mut filter = Filter::new();
    let relayed = bind(
        &elsewhere.blob(),
        &other_session,
        &elsewhere.sign(&other_session),
        false,
    );
    let (answer, outcome) = filter.answer(&relayed, &w.fake);
    assert_eq!(answer, request::failure());
    assert_eq!(
        outcome,
        Outcome::Refused(Refusal::HostKeyChanged {
            kept: w.host.fingerprint(),
            shown: elsewhere.fingerprint(),
        })
    );
    let data = login(&other_session, &w.user.blob(), Some(&elsewhere.blob()));
    assert_eq!(
        filter.answer(&sign_request(&w.user.blob(), &data, 0), &w.fake),
        refused(Refusal::NotBound)
    );
    assert!(w.fake.signed.borrow().is_empty());
    // The first host key still binds.
    let _again = bound(&w);
}

#[test]
fn the_host_key_is_kept_only_for_a_bind_that_holds() {
    let w = world();
    let mut filter = Filter::new();
    // The seen signature with a session id of the guest's choosing.
    let unsigned = bind(&w.host.blob(), &[0x99; 32], &w.host.sign(&SESSION), false);
    let (_, outcome) = filter.answer(&unsigned, &w.fake);
    assert_eq!(outcome, Outcome::Refused(Refusal::BadBindSignature));
    assert!(w.fake.pinned.borrow().is_empty());
    let _bound = bound(&w);
    assert_eq!(
        w.fake.pinned.borrow().get("build.example"),
        Some(&w.host.fingerprint())
    );
}

#[test]
fn a_host_key_that_cannot_be_checked_binds_nothing() {
    let mut w = world();
    w.fake.pin_down = true;
    let mut filter = Filter::new();
    let (_, outcome) = filter.answer(&the_bind(&w), &w.fake);
    assert_eq!(outcome, Outcome::Refused(Refusal::HostKeyUnchecked));
    assert_eq!(filter.host(), None);
}
