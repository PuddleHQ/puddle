// SPDX-License-Identifier: GPL-3.0-or-later
//! The development certificate's Windows side: the current user's `My` store (certificates with
//! keys), the `Root` store (what is trusted, and the one place a trust request goes), and key
//! export through CNG. Part of the crate's `unsafe`, like `windows.rs`.
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
    CERT_CONTEXT, CERT_KEY_PROV_INFO_PROP_ID, CERT_KEY_SPEC, CERT_NCRYPT_KEY_SPEC,
    CERT_STORE_ADD_USE_EXISTING, CERT_STORE_OPEN_EXISTING_FLAG, CERT_STORE_PROV_SYSTEM_W,
    CERT_STORE_READONLY_FLAG, CERT_SYSTEM_STORE_CURRENT_USER, CRYPT_ACQUIRE_ONLY_NCRYPT_KEY_FLAG,
    CRYPT_ACQUIRE_SILENT_FLAG, CertAddCertificateContextToStore, CertCloseStore,
    CertCreateCertificateContext, CertEnumCertificatesInStore, CertFreeCertificateContext,
    CertGetCertificateContextProperty, CertOpenStore, CryptAcquireCertificatePrivateKey,
    HCERTSTORE, NCRYPT_ALLOW_PLAINTEXT_EXPORT_FLAG, NCRYPT_EXPORT_POLICY_PROPERTY,
    NCRYPT_PKCS8_PRIVATE_KEY_BLOB, NCryptExportKey, NCryptFreeObject, NCryptGetProperty,
    X509_ASN_ENCODING,
};
use zeroize::Zeroizing;

use crate::devcert::leaf;
use crate::devcert::{DevCertTrust, ExistingCert, KeyAccess, PrivateKey, TrustError, TrustOutcome};

/// [`DevCertTrust`] over the current user's Windows certificate stores.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsTrust;

fn failed(what: &str, code: u32) -> TrustError {
    TrustError::Failed(format!("{what}: os error {code:#x}"))
}

/// An open certificate store, closed on drop.
struct Store(HCERTSTORE);

impl Store {
    /// The current user's store `name` (`My`, `Root`); `write` opens it for adding.
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

    /// Whether Windows knows a key for the certificate.
    #[expect(unsafe_code, reason = "CryptoAPI property query")]
    fn has_key_info(&self) -> bool {
        let mut size = 0_u32;
        // SAFETY: the context is valid; a null data pointer asks only for the size.
        let found = unsafe {
            CertGetCertificateContextProperty(
                self.0,
                CERT_KEY_PROV_INFO_PROP_ID,
                ptr::null_mut(),
                &raw mut size,
            )
        };
        found != 0
    }

    /// What can be done with the certificate's key, without ever prompting (silent).
    #[expect(unsafe_code, reason = "CNG key handle and export policy")]
    fn key_access(&self) -> KeyAccess {
        if !self.has_key_info() {
            return KeyAccess::Missing;
        }
        match self.open_key() {
            Err(code) => KeyAccess::NotExportable(format!(
                "the key can't be opened without a prompt (os error {code:#x}); it may be on a legacy provider or a smart card"
            )),
            Ok(key) => {
                let mut policy = 0_u32;
                let mut got = 0_u32;
                // SAFETY: `key.0` is an open key handle; the output is the 4 bytes of `policy`.
                let status = unsafe {
                    NCryptGetProperty(
                        key.0,
                        NCRYPT_EXPORT_POLICY_PROPERTY,
                        (&raw mut policy).cast::<u8>(),
                        4,
                        &raw mut got,
                        0,
                    )
                };
                if status != 0 {
                    KeyAccess::NotExportable(format!(
                        "the key's export policy can't be read (status {status:#x})"
                    ))
                } else if policy & NCRYPT_ALLOW_PLAINTEXT_EXPORT_FLAG == 0 {
                    KeyAccess::NotExportable(
                        "the key is not marked exportable in plain form".to_owned(),
                    )
                } else {
                    KeyAccess::Exportable
                }
            }
        }
    }

    /// Opens the certificate's CNG key silently. Legacy (CAPI) keys fail here, which counts as
    /// not exportable.
    #[expect(unsafe_code, reason = "CryptoAPI key acquisition")]
    fn open_key(&self) -> Result<KeyHandle, u32> {
        let mut handle = 0_usize;
        let mut spec: CERT_KEY_SPEC = 0;
        let mut caller_frees = 0;
        // SAFETY: the context is valid; the three out pointers are valid for writes. Silent: no
        // PIN or consent window can appear. Only CNG keys are asked for.
        let ok = unsafe {
            CryptAcquireCertificatePrivateKey(
                self.0,
                CRYPT_ACQUIRE_ONLY_NCRYPT_KEY_FLAG | CRYPT_ACQUIRE_SILENT_FLAG,
                ptr::null(),
                &raw mut handle,
                &raw mut spec,
                &raw mut caller_frees,
            )
        };
        if ok == 0 || spec != CERT_NCRYPT_KEY_SPEC {
            // SAFETY: no preconditions; reads the last error of the call just made.
            return Err(unsafe { GetLastError() });
        }
        Ok(KeyHandle(handle, caller_frees != 0))
    }
}

/// A CNG key handle; freed on drop when the call that gave it says the caller must.
struct KeyHandle(usize, bool);

impl Drop for KeyHandle {
    #[expect(unsafe_code, reason = "CNG handle release")]
    fn drop(&mut self) {
        if self.1 {
            // SAFETY: the handle was handed out for the caller to free, once.
            let _ = unsafe { NCryptFreeObject(self.0) };
        }
    }
}

/// The certificate in `store` whose DER is `der`, handed to `f`.
fn with_cert<T>(store: &Store, der: &[u8], f: impl FnOnce(&CertContext) -> T) -> Option<T> {
    let mut f = Some(f);
    let mut out = None;
    store.each(|cert| {
        if cert.der() == der {
            out = f.take().map(|f| f(cert));
            return false;
        }
        true
    });
    out
}

/// Every certificate of `my` that carries the ASP.NET marker, with what is known about its key
/// and whether `roots` (none: no trusted roots store) holds it. Other certificates are left alone:
/// their keys are never opened.
fn existing_in(my: &Store, roots: Option<&Store>) -> Vec<ExistingCert> {
    let mut found = Vec::new();
    my.each(|cert| {
        let der = cert.der();
        let is_dev_cert = leaf::inspect(der).is_ok_and(|info| info.marker_version.is_some());
        if is_dev_cert {
            found.push(ExistingCert {
                der: der.to_vec(),
                key: cert.key_access(),
                trusted: roots.is_some_and(|roots| roots.contains(der)),
            });
        }
        true
    });
    found
}

#[expect(unsafe_code, reason = "CNG key export")]
fn export_pkcs8(cert: &CertContext) -> Result<PrivateKey, TrustError> {
    if cert.key_access() != KeyAccess::Exportable {
        return Err(TrustError::Failed(
            "the key is not exportable in plain form".into(),
        ));
    }
    let key = cert
        .open_key()
        .map_err(|code| failed("opening the key", code))?;
    let mut size = 0_u32;
    // SAFETY: `key.0` is an open key handle; a null output asks for the size. The blob type is the
    // documented PKCS#8 one and the parameter list may be null for an unencrypted export.
    let status = unsafe {
        NCryptExportKey(
            key.0,
            0,
            NCRYPT_PKCS8_PRIVATE_KEY_BLOB,
            ptr::null(),
            ptr::null_mut(),
            0,
            &raw mut size,
            0,
        )
    };
    if status != 0 {
        return Err(failed("sizing the key export", status.cast_unsigned()));
    }
    let mut buffer = Zeroizing::new(vec![0_u8; size as usize]);
    let mut written = 0_u32;
    // SAFETY: as above; `buffer` has `size` writable bytes and `written` receives how many were
    // used.
    let status = unsafe {
        NCryptExportKey(
            key.0,
            0,
            NCRYPT_PKCS8_PRIVATE_KEY_BLOB,
            ptr::null(),
            buffer.as_mut_ptr(),
            size,
            &raw mut written,
            0,
        )
    };
    if status != 0 {
        return Err(failed("exporting the key", status.cast_unsigned()));
    }
    buffer.truncate(written as usize);
    Ok(PrivateKey::from_pkcs8_der(std::mem::take(&mut *buffer)))
}

fn open(name: &str, write: bool) -> Result<Option<Store>, TrustError> {
    Store::current_user(name, write)
        .map_err(|code| failed(&format!("opening the {name} store"), code))
}

impl DevCertTrust for WindowsTrust {
    fn existing(&self) -> Result<Vec<ExistingCert>, TrustError> {
        let Some(my) = open("My", false)? else {
            return Ok(Vec::new());
        };
        let roots = open("Root", false)?;
        Ok(existing_in(&my, roots.as_ref()))
    }

    fn export_key(&self, cert_der: &[u8]) -> Result<PrivateKey, TrustError> {
        let my = open("My", false)?
            .ok_or_else(|| TrustError::Failed("the personal certificate store is empty".into()))?;
        with_cert(&my, cert_der, export_pkcs8).unwrap_or_else(|| {
            Err(TrustError::Failed(
                "the certificate is not in the store".into(),
            ))
        })
    }

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
mod tests {
    use std::time::SystemTime;

    use super::*;

    fn leaf_der() -> Vec<u8> {
        leaf::generate(SystemTime::now()).unwrap().cert_der
    }

    #[test]
    fn adding_to_a_store_trusts_once_and_keeps_one_copy() {
        let store = Store::memory().unwrap();
        let der = leaf_der();
        assert!(!store.contains(&der));
        assert_eq!(store.add(&der).unwrap(), TrustOutcome::Trusted);
        assert_eq!(store.add(&der).unwrap(), TrustOutcome::Trusted);
        assert!(store.contains(&der));
        let mut count = 0;
        store.each(|_| {
            count += 1;
            true
        });
        assert_eq!(count, 1, "an equal certificate is kept, not duplicated");
        assert!(store.add(b"not a certificate").is_err());
    }

    #[test]
    fn only_certificates_with_the_marker_are_listed_and_trust_is_looked_up() {
        let (my, roots) = (Store::memory().unwrap(), Store::memory().unwrap());
        let dev = leaf_der();
        let plain = {
            let mut p = rcgen::CertificateParams::new(vec!["example.test".to_owned()]).unwrap();
            p.is_ca = rcgen::IsCa::NoCa;
            let key = rcgen::KeyPair::generate().unwrap();
            p.self_signed(&key).unwrap().der().to_vec()
        };
        my.add(&dev).unwrap();
        my.add(&plain).unwrap();
        let listed = existing_in(&my, Some(&roots));
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].der, dev);
        assert!(!listed[0].trusted);
        assert_eq!(
            listed[0].key,
            KeyAccess::Missing,
            "a certificate with no key"
        );

        roots.add(&dev).unwrap();
        assert!(existing_in(&my, Some(&roots))[0].trusted);
        assert!(!existing_in(&my, None)[0].trusted);
    }
}
