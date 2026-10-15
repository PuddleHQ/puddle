// SPDX-License-Identifier: GPL-3.0-or-later
//! Where the development certificate stands, for the first-run flow's certificates step: the
//! [`DevCertService`] trait the route calls, [`NoDevCert`] when none is wired, and [`FakeDevCert`]
//! for tests and the UI fixture.
//!
//! The certificate itself is made and trusted by `puddle-certs`; the host wires the real service,
//! so this crate never names a certificate store.

use std::sync::{Mutex, PoisonError};

use crate::wire::DevCertificate;

/// Tells where the development certificate stands.
pub trait DevCertService: Send + Sync {
    /// The state now. It only reads: it makes no certificate and asks the user nothing. It may
    /// touch the system's certificate stores, so the route calls it off the async threads.
    fn status(&self) -> DevCertificate;
}

/// The service when none is wired in: always "not checked".
#[derive(Debug, Default, Clone, Copy)]
pub struct NoDevCert;

impl DevCertService for NoDevCert {
    fn status(&self) -> DevCertificate {
        DevCertificate::NotChecked
    }
}

/// An in-memory [`DevCertService`] for tests and the UI fixture: it serves the state it was
/// given, and [`FakeDevCert::set`] replaces it.
#[derive(Debug)]
pub struct FakeDevCert {
    status: Mutex<DevCertificate>,
}

impl FakeDevCert {
    /// Serves `status` until [`FakeDevCert::set`] says otherwise.
    #[must_use]
    pub fn new(status: DevCertificate) -> Self {
        Self {
            status: Mutex::new(status),
        }
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
}
