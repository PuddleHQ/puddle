// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_types::fixture::{self, password};

use super::*;

fn proxy(port: u16) -> ProxyAddr {
    ProxyAddr::new("proxy.corp", port)
}

#[test]
fn basic_answers_a_basic_challenge_once() {
    let secret = password("alice");
    let auth = BasicAuth::new().with_default(Credentials::new("alice", &secret));
    let mut session = auth.begin(&proxy(1), &["Basic"]).unwrap().unwrap();
    assert_eq!(
        session.step(Some("Basic realm=\"x\"")).unwrap(),
        AuthStep::Authorization(format!(
            "Basic {}",
            BASE64.encode(format!("alice:{secret}"))
        ))
    );
    assert!(matches!(
        session.step(Some("Basic realm=\"x\"")),
        Err(AuthError::Failed(_))
    ));
}

#[test]
fn basic_is_never_preemptive_and_needs_the_scheme_offered() {
    let auth = BasicAuth::new().with_default(Credentials::new("u", password("u")));
    assert!(auth.begin(&proxy(1), &[]).unwrap().is_none());
    assert!(
        auth.begin(&proxy(1), &["NTLM", "Negotiate"])
            .unwrap()
            .is_none()
    );
    assert!(auth.begin(&proxy(1), &["bAsIc"]).unwrap().is_some());
}

#[test]
fn a_proxy_without_credentials_gets_none_and_its_own_beats_the_default() {
    let (own_password, default_password) = (password("own"), password("default"));
    let auth = BasicAuth::new().with_proxy(proxy(1), Credentials::new("a", &own_password));
    assert!(auth.begin(&proxy(2), &["Basic"]).unwrap().is_none());
    let both = auth.with_default(Credentials::new("d", &default_password));
    let mut own = both.begin(&proxy(1), &["Basic"]).unwrap().unwrap();
    assert_eq!(
        own.step(None).unwrap(),
        AuthStep::Authorization(format!(
            "Basic {}",
            BASE64.encode(format!("a:{own_password}"))
        ))
    );
    let mut other = both.begin(&proxy(9), &["Basic"]).unwrap().unwrap();
    assert_eq!(
        other.step(None).unwrap(),
        AuthStep::Authorization(format!(
            "Basic {}",
            BASE64.encode(format!("d:{default_password}"))
        ))
    );
}

#[derive(Debug)]
struct Fixed(HashMap<ProxyAddr, Credentials>);

impl CredentialSource for Fixed {
    fn credentials(&self, proxy: &ProxyAddr) -> Option<Credentials> {
        self.0.get(proxy).cloned()
    }
}

#[test]
fn a_source_answers_per_proxy_between_the_hand_set_and_the_default() {
    let (s1, s2, by_hand, default) = (password("s1"), password("s2"), password("h"), password("d"));
    let source = Fixed(HashMap::from([
        (proxy(1), Credentials::new("from-source", &s1)),
        (proxy(2), Credentials::new("from-source", &s2)),
    ]));
    let auth = BasicAuth::new()
        .with_proxy(proxy(1), Credentials::new("by-hand", &by_hand))
        .with_default(Credentials::new("default", &default))
        .with_source(Arc::new(source));
    assert_eq!(auth.methods(), ["basic"]);
    let header = |proxy: &ProxyAddr| {
        let mut session = auth.begin(proxy, &["Basic"]).unwrap().unwrap();
        session.step(None).unwrap()
    };
    let basic = |text: &str| AuthStep::Authorization(format!("Basic {}", BASE64.encode(text)));
    assert_eq!(header(&proxy(1)), basic(&format!("by-hand:{by_hand}")));
    assert_eq!(header(&proxy(2)), basic(&format!("from-source:{s2}")));
    assert_eq!(header(&proxy(3)), basic(&format!("default:{default}")));
    // A source alone still offers Basic, and gives nothing for a proxy it does not know.
    let alone = BasicAuth::new().with_source(Arc::new(Fixed(HashMap::new())));
    assert_eq!(alone.methods(), ["basic"]);
    assert!(alone.begin(&proxy(1), &["Basic"]).unwrap().is_none());
}

#[test]
fn debug_never_shows_the_password() {
    let secret = fixture::canary("debug");
    let credentials = Credentials::new("user", &secret);
    let auth = BasicAuth::new().with_default(credentials.clone());
    let session = auth.begin(&proxy(1), &["Basic"]).unwrap().unwrap();
    for text in [
        format!("{credentials:?}"),
        format!("{auth:?}"),
        format!("{session:?}"),
    ] {
        fixture::assert_absent("a debug rendering", &text, &secret);
        fixture::assert_absent(
            "a debug rendering (encoded)",
            &text,
            &BASE64.encode(format!("user:{secret}")),
        );
    }
}

#[derive(Debug, Default)]
struct Counts(std::sync::atomic::AtomicUsize);

impl ProxyAuth for Counts {
    fn begin(
        &self,
        _proxy: &ProxyAddr,
        _offered: &[&str],
    ) -> Result<Option<Box<dyn AuthSession>>, AuthError> {
        Ok(None)
    }

    fn network_changed(&self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[test]
fn a_network_change_reaches_every_member_of_the_list() {
    let (a, b) = (Arc::new(Counts::default()), Arc::new(Counts::default()));
    let list = AuthList::new().with(a.clone()).with(b.clone());
    list.network_changed();
    list.network_changed();
    // Neither member signs in to anything.
    assert!(list.begin(&proxy(1), &["Basic"]).unwrap().is_none());
    assert_eq!(a.0.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(b.0.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[test]
fn the_list_uses_the_first_member_that_helps() {
    let (one, two) = (password("1"), password("2"));
    let first = BasicAuth::new().with_proxy(proxy(1), Credentials::new("first", &one));
    let second = BasicAuth::new().with_default(Credentials::new("second", &two));
    let list = AuthList::new().with(Arc::new(first)).with(Arc::new(second));
    let mut a = list.begin(&proxy(1), &["Basic"]).unwrap().unwrap();
    assert_eq!(
        a.step(None).unwrap(),
        AuthStep::Authorization(format!("Basic {}", BASE64.encode(format!("first:{one}"))))
    );
    let mut b = list.begin(&proxy(2), &["Basic"]).unwrap().unwrap();
    assert_eq!(
        b.step(None).unwrap(),
        AuthStep::Authorization(format!("Basic {}", BASE64.encode(format!("second:{two}"))))
    );
    assert!(
        AuthList::new()
            .begin(&proxy(1), &["Basic"])
            .unwrap()
            .is_none()
    );
}
