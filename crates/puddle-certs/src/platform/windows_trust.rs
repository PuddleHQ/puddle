// SPDX-License-Identifier: AGPL-3.0-or-later
//! The development certificate's Windows side: the current user's `Root` store (what is trusted,
//! and the one place a trust request goes). Part of the crate's `unsafe`, like `windows.rs`.
//!
//! The user's `CurrentUser\Root` is opened by name, so it is the merged view Windows itself and
//! `dotnet dev-certs` use (the user's own roots plus machine, Group Policy and enterprise ones).
//! Adding to it makes Windows ask the user once; a policy that forbids it fails the add and is
//! reported, never worked around.

use std::ffi::c_void;
use std::ptr;

use windows_sys::Win32::Foundation::{
    ERROR_CANCELLED, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, GetLastError,
};
use windows_sys::Win32::Security::Cryptography::{
    CERT_CONTEXT, CERT_STORE_ADD_USE_EXISTING, CERT_STORE_OPEN_EXISTING_FLAG,
    CERT_STORE_PROV_SYSTEM_W, CERT_STORE_READONLY_FLAG, CERT_SYSTEM_STORE_CURRENT_USER,
    CertAddCertificateContextToStore, CertCloseStore, CertCreateCertificateContext,
    CertEnumCertificatesInStore, CertFreeCertificateContext, CertOpenStore, HCERTSTORE,
    X509_ASN_ENCODING,
};

use crate::devcert::{DevCertTrust, TrustError, TrustOutcome};

/// [`DevCertTrust`] over the current user's Windows certificate stores.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsTrust;

fn failed(what: &str, code: u32) -> TrustError {
    TrustError::Failed(format!("{what}: os error {code:#x}"))
}

/// An open certificate store, closed on drop.
struct Store(HCERTSTORE);

impl Store {
    /// The current user's store `name` (`Root`, or a memory store in tests); `write` opens it for adding.
    #[expect(unsafe_code, reason = "CryptoAPI store handle")]
    fn current_user(name: &str, write: bool) -> Result<Option<Self>, u32> {
        let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
        let flags = CERT_SYSTEM_STORE_CURRENT_USER
            | if write {
                0
            } else {
                CERT_STORE_READONLY_FLAG | CERT_STORE_OPEN_EXISTING_FLAG
            };
        // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call, which is what
        // the system-store provider takes as `pvPara`; encoding type and provider are the
        // documented defaults (0).
        let handle = unsafe {
            CertOpenStore(
                CERT_STORE_PROV_SYSTEM_W,
                0,
                0,
                flags,
                wide.as_ptr().cast::<c_void>(),
            )
        };
        Self::checked(handle)
    }

    /// An empty store in memory (tests: no dialog, no machine state).
    #[cfg(test)]
    #[expect(unsafe_code, reason = "CryptoAPI store handle")]
    fn memory() -> Result<Self, u32> {
        // SAFETY: the memory provider takes no parameter; the other arguments are the defaults.
        let handle = unsafe {
            CertOpenStore(
                windows_sys::Win32::Security::Cryptography::CERT_STORE_PROV_MEMORY,
                0,
                0,
                0,
                ptr::null(),
            )
        };
        Self::checked(handle)?.ok_or(0)
    }

    #[expect(
        unsafe_code,
        reason = "reads the thread's last error right after a failed call"
    )]
    fn checked(handle: HCERTSTORE) -> Result<Option<Self>, u32> {
        if !handle.is_null() {
            return Ok(Some(Self(handle)));
        }
        // SAFETY: no preconditions; reads this thread's last error set by the call just made.
        let code = unsafe { GetLastError() };
        // A store that does not exist yet (nothing ever added) is an empty one.
        if code == ERROR_FILE_NOT_FOUND || code == ERROR_PATH_NOT_FOUND {
            return Ok(None);
        }
        Err(code)
    }

    /// Calls `visit` with every certificate context in the store; it returns `false` to stop.
    #[expect(unsafe_code, reason = "CryptoAPI certificate enumeration")]
    fn each(&self, mut visit: impl FnMut(&CertContext) -> bool) {
        let mut previous: *const CERT_CONTEXT = ptr::null();
        loop {
            // SAFETY: `self.0` is an open store; `previous` is null or the context the last call
            // returned, which this call frees (the enumeration's contract).
            let next = unsafe { CertEnumCertificatesInStore(self.0, previous) };
            if next.is_null() {
                return;
            }
            previous = next;
            if !visit(&CertContext(next)) {
                // SAFETY: `next` is a live context from the enumeration that is not enumerated
                // further, so it is freed here exactly once.
                unsafe { CertFreeCertificateContext(next) };
                return;
            }
        }
    }

    /// Adds `der` (replacing nothing: an equal certificate already there is kept).
    #[expect(unsafe_code, reason = "CryptoAPI certificate add")]
    fn add(&self, der: &[u8]) -> Result<TrustOutcome, TrustError> {
        let len = u32::try_from(der.len()).map_err(|_| failed("certificate too large", 0))?;
        // SAFETY: `der` is `len` readable bytes; the call copies them into a new context.
        let context = unsafe { CertCreateCertificateContext(X509_ASN_ENCODING, der.as_ptr(), len) };
        if context.is_null() {
            // SAFETY: no preconditions; reads the last error of the call just made.
            return Err(failed("reading the certificate", unsafe { GetLastError() }));
        }
        // SAFETY: `self.0` is an open writable store and `context` the context made above, which
        // the add does not take over; the out pointer may be null.
        let added = unsafe {
            CertAddCertificateContextToStore(
                self.0,
                context,
                CERT_STORE_ADD_USE_EXISTING,
                ptr::null_mut(),
            )
        };
        // SAFETY: no preconditions; reads the last error before anything else can change it.
        let code = unsafe { GetLastError() };
        // SAFETY: `context` was created above and is freed once.
        unsafe { CertFreeCertificateContext(context) };
        if added != 0 {
            Ok(TrustOutcome::Trusted)
        } else if code == ERROR_CANCELLED {
            Ok(TrustOutcome::Declined)
        } else {
            Err(failed("adding the certificate to the trusted roots", code))
        }
    }

    fn contains(&self, der: &[u8]) -> bool {
        let mut found = false;
        self.each(|cert| {
            found = cert.der() == der;
            !found
        });
        found
    }
}

impl Drop for Store {
    #[expect(unsafe_code, reason = "CryptoAPI store handle")]
    fn drop(&mut self) {
        // SAFETY: `self.0` came from a successful CertOpenStore and is closed exactly once.
        // Flags 0: every context handed out was freed.
        let _ = unsafe { CertCloseStore(self.0, 0) };
    }
}

/// A certificate context borrowed from an enumeration (valid until the next step).
struct CertContext(*const CERT_CONTEXT);

impl CertContext {
    #[expect(unsafe_code, reason = "reads the encoded certificate")]
    fn der(&self) -> &[u8] {
        // SAFETY: a context from the enumeration is valid for this borrow; `pbCertEncoded` points
        // at `cbCertEncoded` bytes it owns.
        unsafe {
            let c = &*self.0;
            std::slice::from_raw_parts(c.pbCertEncoded, c.cbCertEncoded as usize)
        }
    }
}

fn open(name: &str, write: bool) -> Result<Option<Store>, TrustError> {
    Store::current_user(name, write)
        .map_err(|code| failed(&format!("opening the {name} store"), code))
}

impl DevCertTrust for WindowsTrust {
    fn is_trusted(&self, cert_der: &[u8]) -> Result<bool, TrustError> {
        Ok(open("Root", false)?.is_some_and(|roots| roots.contains(cert_der)))
    }

    fn trust(&self, cert_der: &[u8]) -> Result<TrustOutcome, TrustError> {
        let roots = open("Root", true)?
            .ok_or_else(|| TrustError::Failed("the trusted roots store is unavailable".into()))?;
        roots.add(cert_der)
    }
}

#[cfg(test)]
mod tests;
