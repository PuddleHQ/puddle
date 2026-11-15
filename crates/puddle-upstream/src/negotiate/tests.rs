// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::atomic::{AtomicUsize, Ordering};

use proptest::prelude::*;

use super::*;

type Log = Arc<Mutex<Vec<(Package, Option<Vec<u8>>)>>>;

/// How a scripted context behaves.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Type 1, then Type 3 once it sees a challenge.
    #[default]
    Ntlm,
    /// Kerberos-like on Negotiate: complete after the first token.
    OneShot,
    /// Type 1, then complete without a token (a mutual-auth acknowledgement).
    Ack,
    /// Every step fails.
    FailStep,
    /// Every step returns nothing and is not complete.
    Silent,
    /// The source cannot open a context at all.
    FailOpen,
}

#[derive(Debug)]
struct Script {
    package: Package,
    calls: usize,
    log: Log,
    mode: Mode,
}

impl SecurityContext for Script {
    fn step(&mut self, input: Option<&[u8]>) -> Result<Leg, AuthError> {
        self.calls += 1;
        self.log
            .lock()
            .unwrap()
            .push((self.package, input.map(<[u8]>::to_vec)));
        let leg = |token: &[u8], complete| Leg {
            token: token.to_vec(),
            complete,
        };
        match (self.calls, self.mode) {
            (_, Mode::FailStep) => Err(AuthError::Failed("scripted failure".into())),
            (_, Mode::Silent) => Ok(leg(b"", false)),
            (1, Mode::OneShot) if self.package == Package::Negotiate => Ok(leg(b"krb", true)),
            (1, _) => Ok(leg(b"type1", false)),
            (2, Mode::Ack) => Ok(leg(b"", true)),
            (2, _) => Ok(leg(b"type3", true)),
            _ => Err(AuthError::Failed("scripted: too many calls".into())),
        }
    }
}

#[derive(Debug, Default)]
struct Source {
    opened: AtomicUsize,
    mode: Mode,
    log: Log,
}

impl TokenSource for Source {
    fn open(&self, package: Package, spn: &str) -> Result<Box<dyn SecurityContext>, AuthError> {
        assert_eq!(spn, "HTTP/proxy.corp");
        if self.mode == Mode::FailOpen {
            return Err(AuthError::Failed("no credentials".into()));
        }
        self.opened.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Script {
            package,
            calls: 0,
            log: Arc::clone(&self.log),
            mode: self.mode,
        }))
    }
}

fn auth(source: Source) -> (NegotiateAuth, Arc<Source>) {
    let source = Arc::new(source);
    (
        NegotiateAuth::new(Arc::clone(&source) as Arc<dyn TokenSource>),
        source,
    )
}

fn proxy() -> ProxyAddr {
    ProxyAddr::new("Proxy.Corp", 8080)
}

fn header(scheme: &str, token: &[u8]) -> String {
    format!("{scheme} {}", STANDARD.encode(token))
}

fn authorization(step: AuthStep) -> String {
    match step {
        AuthStep::Authorization(value) => value,
        other => panic!("expected a header, got {other:?}"),
    }
}

#[test]
fn a_preemptive_negotiate_token_needs_no_challenge() {
    let (auth, _) = auth(Source {
        mode: Mode::OneShot,
        ..Source::default()
    });
    let mut session = auth.begin(&proxy(), &[]).unwrap().unwrap();
    assert_eq!(
        authorization(session.step(None).unwrap()),
        header("Negotiate", b"krb")
    );
}

#[test]
fn ntlm_runs_three_legs() {
    let (auth, source) = auth(Source::default());
    let mut session = auth.begin(&proxy(), &["NTLM"]).unwrap().unwrap();
    assert_eq!(
        authorization(session.step(Some("NTLM")).unwrap()),
        header("NTLM", b"type1")
    );
    let type2 = header("NTLM", b"type2");
    assert_eq!(
        authorization(session.step(Some(&type2)).unwrap()),
        header("NTLM", b"type3")
    );
    let log = source.log.lock().unwrap();
    assert_eq!(log[0], (Package::Ntlm, None));
    assert_eq!(log[1], (Package::Ntlm, Some(b"type2".to_vec())));
}

#[test]
fn negotiate_is_preferred_and_the_first_call_may_be_the_bare_407() {
    let (auth, source) = auth(Source::default());
    let mut session = auth
        .begin(&proxy(), &["NTLM", "Negotiate"])
        .unwrap()
        .unwrap();
    let sent = authorization(session.step(Some("Negotiate, NTLM")).unwrap());
    assert!(sent.starts_with("Negotiate "));
    assert_eq!(source.opened.load(Ordering::SeqCst), 1);
}

#[test]
fn the_scheme_the_407_names_decides_over_the_default() {
    // A preemptive session starts on Negotiate; the proxy then only speaks NTLM.
    let (auth, source) = auth(Source::default());
    let mut session = auth.begin(&proxy(), &[]).unwrap().unwrap();
    let sent = authorization(session.step(Some("NTLM")).unwrap());
    assert_eq!(sent, header("NTLM", b"type1"));
    assert_eq!(source.opened.load(Ordering::SeqCst), 2);
}

#[test]
fn a_refused_negotiate_token_restarts_on_ntlm_once() {
    let (auth, _) = auth(Source::default());
    let mut session = auth.begin(&proxy(), &[]).unwrap().unwrap();
    assert!(authorization(session.step(None).unwrap()).starts_with("Negotiate "));
    // 407 again, bare: the proxy did not take the token, and still offers NTLM.
    let sent = authorization(session.step(Some("Negotiate, NTLM")).unwrap());
    assert_eq!(sent, header("NTLM", b"type1"));
    // Refused again with nothing new to try.
    let err = session.step(Some("Negotiate, NTLM")).unwrap_err();
    assert!(err.to_string().contains("refused"), "{err}");
}

#[test]
fn a_refusal_with_nothing_left_is_an_error_not_a_loop() {
    let (auth, _) = auth(Source {
        mode: Mode::OneShot,
        ..Source::default()
    });
    let mut session = auth.begin(&proxy(), &[]).unwrap().unwrap();
    session.step(None).unwrap();
    assert!(
        session
            .step(Some("Negotiate"))
            .unwrap_err()
            .to_string()
            .contains("refused")
    );
}

#[test]
fn an_established_context_with_nothing_to_send_is_done() {
    let (auth, _) = auth(Source {
        mode: Mode::Ack,
        ..Source::default()
    });
    let mut session = auth.begin(&proxy(), &["NTLM"]).unwrap().unwrap();
    session.step(Some("NTLM")).unwrap();
    let type2 = header("NTLM", b"t2");
    assert_eq!(session.step(Some(&type2)).unwrap(), AuthStep::Done);
    // A 407 after completion is a refusal.
    assert!(session.step(Some(&type2)).is_err());
}

#[test]
fn schemes_this_module_cannot_speak_are_not_offered_a_session() {
    let (auth, source) = auth(Source::default());
    assert!(auth.begin(&proxy(), &["Basic"]).unwrap().is_none());
    assert!(
        auth.begin(&proxy(), &["Basic", "Digest"])
            .unwrap()
            .is_none()
    );
    assert_eq!(source.opened.load(Ordering::SeqCst), 0);
    assert!(auth.begin(&proxy(), &["negotiate"]).unwrap().is_some());
}

#[test]
fn a_challenge_without_a_known_scheme_is_an_error() {
    let (auth, _) = auth(Source::default());
    let mut session = auth.begin(&proxy(), &[]).unwrap().unwrap();
    assert!(session.step(Some("Basic realm=\"corp\"")).is_err());
}

#[test]
fn a_basic_only_proxy_is_not_sent_a_preemptive_token_again() {
    let (auth, source) = auth(Source::default());
    let mut first = auth.begin(&proxy(), &[]).unwrap().unwrap();
    assert!(first.step(Some("Basic realm=\"corp\"")).is_err());
    assert!(auth.begin(&proxy(), &[]).unwrap().is_none());
    // A 407 that does offer NTLM works again.
    assert!(auth.begin(&proxy(), &["NTLM"]).unwrap().is_some());
    assert_eq!(source.opened.load(Ordering::SeqCst), 2);
}

#[test]
fn a_network_change_forgets_what_each_proxy_asked_for() {
    let (auth, _) = auth(Source::default());
    let mut first = auth.begin(&proxy(), &[]).unwrap().unwrap();
    assert!(first.step(Some("Basic realm=\"corp\"")).is_err());
    assert!(auth.begin(&proxy(), &[]).unwrap().is_none());
    auth.network_changed();
    // The proxy behind the name may be another one now: the first connection probes again.
    let mut again = auth.begin(&proxy(), &[]).unwrap().unwrap();
    assert_eq!(
        authorization(again.step(None).unwrap()),
        header("Negotiate", b"type1")
    );
}

#[test]
fn a_challenge_that_is_not_base64_is_an_error_without_the_text() {
    let (auth, _) = auth(Source::default());
    let mut session = auth.begin(&proxy(), &["NTLM"]).unwrap().unwrap();
    session.step(Some("NTLM")).unwrap();
    let err = session.step(Some("NTLM not*base64!")).unwrap_err();
    assert!(!err.to_string().contains("base64!"), "{err}");
}

#[test]
fn the_scheme_a_proxy_asked_for_starts_the_next_preemptive_session() {
    let (auth, source) = auth(Source::default());
    let mut first = auth.begin(&proxy(), &[]).unwrap().unwrap();
    first.step(Some("NTLM")).unwrap();
    let mut second = auth.begin(&proxy(), &[]).unwrap().unwrap();
    assert_eq!(
        authorization(second.step(None).unwrap()),
        header("NTLM", b"type1")
    );
    assert_eq!(source.log.lock().unwrap().last().unwrap().0, Package::Ntlm);
}

#[test]
fn a_source_that_cannot_open_fails_begin_and_never_prompts() {
    let (auth, _) = auth(Source {
        mode: Mode::FailOpen,
        ..Source::default()
    });
    assert!(auth.begin(&proxy(), &[]).is_err());
    assert!(auth.begin(&proxy(), &["NTLM"]).is_err());
}

#[test]
fn a_context_error_is_passed_up() {
    let (auth, _) = auth(Source {
        mode: Mode::FailStep,
        ..Source::default()
    });
    let mut session = auth.begin(&proxy(), &["NTLM"]).unwrap().unwrap();
    assert!(session.step(Some("NTLM")).is_err());
}

#[test]
fn a_context_that_is_neither_done_nor_has_a_token_is_an_error() {
    let (auth, _) = auth(Source {
        mode: Mode::Silent,
        ..Source::default()
    });
    let mut session = auth.begin(&proxy(), &["NTLM"]).unwrap().unwrap();
    assert!(
        session
            .step(Some("NTLM"))
            .unwrap_err()
            .to_string()
            .contains("no token")
    );
}

#[test]
fn tokens_stay_out_of_debug_output() {
    let (auth, _) = auth(Source::default());
    let mut session = auth.begin(&proxy(), &["NTLM"]).unwrap().unwrap();
    session.step(Some("NTLM")).unwrap();
    let shown = format!("{session:?} {auth:?}");
    assert!(!shown.contains("type1") && !shown.contains("dHlwZTE"));
}

#[test]
fn parsing_reads_tokens_and_skips_other_schemes() {
    let value = format!(
        "Basic realm=\"x\", NTLM, Negotiate {}",
        STANDARD.encode(b"abc")
    );
    let offers = parse_challenge(&value).unwrap();
    assert_eq!(
        offers,
        vec![
            Offer {
                package: Package::Ntlm,
                token: None
            },
            Offer {
                package: Package::Negotiate,
                token: Some(b"abc".to_vec())
            },
        ]
    );
    assert_eq!(parse_challenge("").unwrap().len(), 0);
    assert_eq!(Package::Ntlm.word(), "NTLM");
}

proptest! {
    #[test]
    fn parsing_never_panics(text in ".*") {
        let _ = parse_challenge(&text);
    }

    #[test]
    fn a_token_round_trips_through_a_header(bytes in proptest::collection::vec(any::<u8>(), 1..200)) {
        let offers = parse_challenge(&header("NTLM", &bytes)).unwrap();
        prop_assert_eq!(offers.len(), 1);
        prop_assert_eq!(offers[0].token.as_deref(), Some(bytes.as_slice()));
    }
}
