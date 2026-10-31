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
    /// threads and with a time limit. A second call after a yes changes nothing.
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
        }
    }
}

/// An in-memory [`DevCertService`] for tests and the UI fixture: it serves the state it was
/// given, and [`FakeDevCert::set`] replaces it. Trusting it answers what
/// [`FakeDevCert::set_trust_answer`] says (a yes by default) and keeps that as the state.
#[derive(Debug)]
pub struct FakeDevCert {
    status: Mutex<DevCertificate>,
    answer: Mutex<DevCertTrustResult>,
    trusts: Mutex<u32>,
}

impl FakeDevCert {
    /// Serves `status` until [`FakeDevCert::set`] says otherwise.
    #[must_use]
    pub fn new(status: DevCertificate) -> Self {
        Self {
            status: Mutex::new(status),
            answer: Mutex::new(DevCertTrustResult {
                dev_certificate: DevCertificate::Trusted,
                detail: None,
            }),
            trusts: Mutex::new(0),
        }
    }

    /// What the next presses of "Trust it now" come to.
    pub fn set_trust_answer(&self, dev_certificate: DevCertificate, detail: Option<String>) {
        *self.answer.lock().unwrap_or_else(PoisonError::into_inner) = DevCertTrustResult {
            dev_certificate,
            detail,
        };
    }

    /// How many times trusting was asked for.
    #[must_use]
    pub fn trust_calls(&self) -> u32 {
        *self.trusts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Serves `status` from now on.
    pub fn set(&self, status: DevCertificate) {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner) = status;
    }
}

impl DevCertService for FakeDevCert {
    fn status(&self) -> DevCertificate {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn trust(&self) -> DevCertTrustResult {
        *self.trusts.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        let answer = self
            .answer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        self.set(answer.dev_certificate);
        answer
    }
}
