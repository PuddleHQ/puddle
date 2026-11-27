// SPDX-License-Identifier: AGPL-3.0-or-later
//! The development certificate end to end: keep one localhost leaf in the data folder and trust
//! it once.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use zeroize::{Zeroize as _, Zeroizing};

use super::leaf::{self, RENEW_WITHIN, check_trust_shape, unix_secs};
use super::material::{DevCertMaterial, PrivateKey};
use super::trust::{DevCertTrust, TrustError, TrustOutcome};

/// The folder under the data folder that holds the leaf.
pub const FOLDER: &str = "dev-cert";

/// The leaf's file: certificate and private key as two PEM blocks, one owner-only file written
/// in one step, so a crash never leaves a certificate beside another key.
pub const LEAF_FILE: &str = "localhost.pem";

/// Where the development certificate stands, for the first-run line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DevCertStatus {
    /// Puddle's own leaf is in use and trusted.
    Trusted,
    /// Puddle's own leaf is not trusted yet; the system will ask the user once.
    NeedsTrust,
    /// The user answered "No" to the system's confirmation.
    TrustDeclined,
    /// Trusting failed (a policy, or the store): see the report's detail.
    TrustFailed,
    /// This host has no trust implementation yet. The leaf is still made for the guests.
    Unsupported,
}

/// What [`DevCertManager::ensure`] did.
#[derive(Debug)]
pub struct DevCertReport {
    /// Where it stands.
    pub status: DevCertStatus,
    /// The certificate and key for the boot plan: always present after a successful run, even
    /// when trust was declined (the guest trusts it on its own).
    pub material: DevCertMaterial,
    /// For [`DevCertStatus::TrustFailed`], what failed.
    pub detail: Option<String>,
}

/// Why the certificate could not be made or kept.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DevCertError {
    /// The data folder could not be written.
    #[error("the development certificate folder {path}: {source}")]
    Folder {
        /// The folder or file.
        path: PathBuf,
        /// The file-system error.
        #[source]
        source: io::Error,
    },
    /// Generating the leaf failed.
    #[error("{0}")]
    Generate(String),
}

/// Keeps the development certificate for one host user. Trust operations go through `T`.
pub struct DevCertManager<T> {
    dir: PathBuf,
    trust: T,
    last: Mutex<Option<(DevCertStatus, String)>>,
}

impl<T> std::fmt::Debug for DevCertManager<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DevCertManager")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl<T: DevCertTrust> DevCertManager<T> {
    /// A manager keeping its files under `data_dir` (the per-user data folder).
    pub fn new(data_dir: &Path, trust: T) -> Self {
        Self {
            dir: data_dir.join(FOLDER),
            trust,
            last: Mutex::new(None),
        }
    }

    /// Where it stands, from what is there now. Reads only: it makes nothing, exports no key and
    /// never asks the user anything.
    pub fn status(&self, now: SystemTime) -> DevCertStatus {
        if !self.trust.is_supported() {
            return DevCertStatus::Unsupported;
        }
        let own_trusted = self
            .load_leaf(now)
            .is_some_and(|(cert, _)| self.trust.is_trusted(&cert) == Ok(true));
        if own_trusted {
            return DevCertStatus::Trusted;
        }
        // What the user last answered stays visible until the next attempt.
        match &*self.last.lock().unwrap_or_else(PoisonError::into_inner) {
            Some((status @ (DevCertStatus::TrustDeclined | DevCertStatus::TrustFailed), _)) => {
                *status
            }
            _ => DevCertStatus::NeedsTrust,
        }
    }

    /// Makes sure a certificate is ready for the guests and, when Puddle's own is used, trusted.
    /// The system's confirmation may appear (once, the first time Puddle's own leaf is trusted).
    /// A declined or failed trust is reported in the status, not as an error.
    ///
    /// # Errors
    ///
    /// The data folder can't be written or the leaf can't be generated.
    pub fn ensure(&self, now: SystemTime) -> Result<DevCertReport, DevCertError> {
        let (cert_der, key) = self.leaf(now)?;
        let (status, detail) = self.trust_leaf(&cert_der);
        Ok(self.finish(status, DevCertMaterial::new(cert_der, key), detail))
    }

    /// Makes sure a certificate is ready for the guests and never asks the user anything: the
    /// status is what the stores say now (a trusted leaf, or [`DevCertStatus::NeedsTrust`] when
    /// it is not, whatever was answered before). A workspace start uses this, so a "No" given on
    /// the first-run screen is not asked again at every start; only [`Self::ensure`] asks.
    ///
    /// # Errors
    ///
    /// The data folder can't be written or the leaf can't be generated.
    pub fn prepare(&self, now: SystemTime) -> Result<DevCertReport, DevCertError> {
        let (cert_der, key) = self.leaf(now)?;
        let (status, detail) = match self.trust.is_trusted(&cert_der) {
            Ok(true) => (DevCertStatus::Trusted, None),
            Ok(false) => (DevCertStatus::NeedsTrust, None),
            Err(TrustError::Unsupported) => (DevCertStatus::Unsupported, None),
            Err(TrustError::Failed(why)) => (DevCertStatus::TrustFailed, Some(why)),
        };
        Ok(DevCertReport {
            status,
            material: DevCertMaterial::new(cert_der, key),
            detail,
        })
    }

    fn finish(
        &self,
        status: DevCertStatus,
        material: DevCertMaterial,
        detail: Option<String>,
    ) -> DevCertReport {
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((status, detail.clone().unwrap_or_default()));
        DevCertReport {
            status,
            material,
            detail,
        }
    }

    fn trust_leaf(&self, cert_der: &[u8]) -> (DevCertStatus, Option<String>) {
        match self.trust.is_trusted(cert_der) {
            Ok(true) => return (DevCertStatus::Trusted, None),
            Ok(false) => {}
            Err(TrustError::Unsupported) => return (DevCertStatus::Unsupported, None),
            Err(TrustError::Failed(why)) => return (DevCertStatus::TrustFailed, Some(why)),
        }
        // Only a certificate of the shape Puddle makes is ever offered to the system.
        if let Err(error) = check_trust_shape(cert_der) {
            return (DevCertStatus::TrustFailed, Some(error.to_string()));
        }
        match self.trust.trust(cert_der) {
            Ok(TrustOutcome::Trusted) => (DevCertStatus::Trusted, None),
            Ok(TrustOutcome::Declined) => (DevCertStatus::TrustDeclined, None),
            Err(TrustError::Unsupported) => (DevCertStatus::Unsupported, None),
            Err(TrustError::Failed(why)) => (DevCertStatus::TrustFailed, Some(why)),
        }
    }

    /// The leaf on disk if it is still good, else a new one, written before it is returned.
    fn leaf(&self, now: SystemTime) -> Result<(Vec<u8>, PrivateKey), DevCertError> {
        if let Some(found) = self.load_leaf(now) {
            return Ok(found);
        }
        let new = leaf::generate(now).map_err(|e| DevCertError::Generate(e.to_string()))?;
        let folder = |source| DevCertError::Folder {
            path: self.dir.clone(),
            source,
        };
        puddle_fs::private::create_dir(&self.dir).map_err(folder)?;
        let mut body = Zeroizing::new(pem::encode(&pem::Pem::new(
            "CERTIFICATE",
            new.cert_der.clone(),
        )));
        body.push_str(&new.key.to_pem());
        puddle_fs::private::write_atomic(&self.dir.join(LEAF_FILE), body.as_bytes())
            .map_err(folder)?;
        Ok((new.cert_der, new.key))
    }

    /// The stored leaf, if the file is owner-only, parses, has the shape and has at least
    /// [`RENEW_WITHIN`] of validity left (and has begun).
    fn load_leaf(&self, now: SystemTime) -> Option<(Vec<u8>, PrivateKey)> {
        let file = fs::File::open(self.dir.join(LEAF_FILE)).ok()?;
        // A key file others can read is a leaked key: replace it, never use it.
        puddle_fs::private::check(&file).ok()?;
        let text = Zeroizing::new(io::read_to_string(&file).ok()?);
        // The key leaves its parsed block without a copy, and every other block is overwritten
        // before it drops, since `Pem` does not do that itself.
        let mut cert = None;
        let mut key = None;
        for block in pem::parse_many(text.as_bytes()).ok()? {
            let tag = block.tag().to_owned();
            if tag == "CERTIFICATE" && cert.is_none() {
                cert = Some(block.into_contents());
            } else if tag == "PRIVATE KEY" && key.is_none() {
                key = Some(PrivateKey::from_pkcs8_der(block.into_contents()));
            } else {
                block.into_contents().zeroize();
            }
        }
        let (key, cert) = (key?, cert?);
        let info = check_trust_shape(&cert).ok()?;
        let now = unix_secs(now);
        let renew = i64::try_from(RENEW_WITHIN.as_secs()).ok()?;
        let good = info.not_before <= now && info.not_after - now >= renew;
        good.then_some((cert, key))
    }
}
