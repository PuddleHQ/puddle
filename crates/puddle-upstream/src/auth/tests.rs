// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[derive(Debug)]
struct Echo(u8);

impl AuthSession for Echo {
    fn step(&mut self, challenge: Option<&str>) -> Result<AuthStep, AuthError> {
        self.0 += 1;
        match (self.0, challenge) {
            (1, None) => Ok(AuthStep::Authorization("Negotiate first".into())),
            (2, Some(c)) => Ok(AuthStep::Authorization(format!("Negotiate answer-{c}"))),
            (_, Some(_)) => Ok(AuthStep::Done),
            _ => Err(AuthError::Failed("unexpected".into())),
        }
    }
}

#[derive(Debug)]
struct Fake;

impl ProxyAuth for Fake {
    fn begin(
        &self,
        _proxy: &ProxyAddr,
        offered: &[&str],
    ) -> Result<Option<Box<dyn AuthSession>>, AuthError> {
        Ok(offered
            .is_empty()
            .then(|| Box::new(Echo(0)) as Box<dyn AuthSession>))
    }
}

#[test]
fn no_auth_never_offers_a_session() {
    let proxy = ProxyAddr::new("p", 1);
    assert!(NoAuth.begin(&proxy, &[]).unwrap().is_none());
    assert!(NoAuth.begin(&proxy, &["Negotiate"]).unwrap().is_none());
    // Basic is never answered by any platform implementation: no password prompt exists.
    assert!(system_auth().begin(&proxy, &["Basic"]).unwrap().is_none());
    #[cfg(not(windows))]
    assert!(system_auth().begin(&proxy, &["NTLM"]).unwrap().is_none());
}

#[test]
fn a_multi_leg_exchange_fits_the_trait() {
    let proxy = ProxyAddr::new("p", 1);
    let mut session = Fake.begin(&proxy, &[]).unwrap().unwrap();
    assert_eq!(
        session.step(None).unwrap(),
        AuthStep::Authorization("Negotiate first".into())
    );
    assert_eq!(
        session.step(Some("c")).unwrap(),
        AuthStep::Authorization("Negotiate answer-c".into())
    );
    assert_eq!(session.step(Some("d")).unwrap(), AuthStep::Done);
    let mut broken = Echo(5);
    assert!(broken.step(None).is_err());
    assert!(Fake.begin(&proxy, &["Basic"]).unwrap().is_none());
}
