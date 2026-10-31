// SPDX-License-Identifier: GPL-3.0-or-later
//! Where the development certificate stands, for the first-run flow's certificates step: the
//! [`DevCertService`] trait the route calls, [`NoDevCert`] when none is wired, and [`FakeDevCert`]
//! for tests and the UI fixture.
//!
//! The certificate itself is made and trusted by `puddle-certs`; the host wires the real service,
//! so this crate never names a certificate store.

use std::sync::{Mutex, PoisonError};

use crate::wire::{DevCertTrustResult, DevCertificate};

/// Tells where the development certificate stands.
pub trait DevCertService: Send + Sync {
    /// The state now. It only reads: it makes no certificate and asks the user nothing. It may
    /// touch the system's certificate stores, so the route calls it off the async threads.
    fn status(&self) -> DevCertificate;

    /// Makes the certificate if needed and asks the system to trust it, now. The system's own
    /// confirmation may open and this waits for the answer, so the route calls it off the async
    /// threads and with a time limit. A second call after a yes changes nothing, and a call while
    /// an earlier one still waits for the user opens no second confirmation: it answers the state
    /// as it stands with `waiting` set.
    fn trust(&self) -> DevCertTrustResult;
}

/// The service when none is wired in: always "not checked".
#[derive(Debug, Default, Clone, Copy)]
pub struct NoDevCert;

impl DevCertService for NoDevCert {
    fn status(&self) -> DevCertificate {
        DevCertificate::NotChecked
    }

    fn trust(&self) -> DevCertTrustResult {
        DevCertTrustResult {
            dev_certificate: DevCertificate::NotChecked,
            detail: None,
            waiting: false,
        }
    }
}

/// An in-memory [`DevCertService`] for tests and the UI fixture: it serves the state it was
/// given, and [`FakeDevCert::set`] replaces it. Trusting it answers what
/// [`FakeDevCert::set_trust_answer`] says (a yes by default) and keeps that as the state.
#[derive(Debug)]
pub struct FakeDevCert {
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    status: DevCertificate,
    answer: DevCertTrustResult,
    trusts: u32,
}

impl FakeDevCert {
    /// Serves `status` until [`FakeDevCert::set`] says otherwise.
    #[must_use]
    pub fn new(status: DevCertificate) -> Self {
        Self {
            inner: Mutex::new(Inner {
                status,
                answer: DevCertTrustResult {
                    dev_certificate: DevCertificate::Trusted,
                    detail: None,
                    waiting: false,
                },
                trusts: 0,
            }),
        }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What the next presses of "Trust it now" come to.
    pub fn set_trust_answer(&self, dev_certificate: DevCertificate, detail: Option<String>) {
        self.inner().answer = DevCertTrustResult {
            dev_certificate,
            detail,
            waiting: false,
        };
    }

    /// How many times trusting was asked for.
    #[must_use]
    pub fn trust_calls(&self) -> u32 {
        self.inner().trusts
    }

    /// Serves `status` from now on.
    pub fn set(&self, status: DevCertificate) {
        self.inner().status = status;
    }
}

impl DevCertService for FakeDevCert {
    fn status(&self) -> DevCertificate {
        self.inner().status
    }

    fn trust(&self) -> DevCertTrustResult {
        let mut inner = self.inner();
        inner.trusts += 1;
        inner.status = inner.answer.dev_certificate;
        inner.answer.clone()
    }
}
