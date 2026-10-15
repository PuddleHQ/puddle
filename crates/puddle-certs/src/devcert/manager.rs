// SPDX-License-Identifier: GPL-3.0-or-later
//! The development certificate end to end: reuse what Windows already trusts, else keep one
//! localhost leaf in the data folder and trust it once.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use zeroize::Zeroizing;

use super::leaf::{self, RENEW_WITHIN, check_trust_shape, unix_secs};
use super::material::{DevCertMaterial, Origin, PrivateKey};
use super::reuse::{SkipReason, SkippedExisting, rank};
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
    /// An ASP.NET development certificate Windows already trusts is in use. No dialog.
    ReusingExisting,
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
    /// Existing development certificates passed over, and why.
    pub skipped: Vec<SkippedExisting>,
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
        match self.trust.existing() {
            Err(TrustError::Unsupported) => return DevCertStatus::Unsupported,
            Ok(existing) if !rank(&existing, now).eligible.is_empty() => {
                return DevCertStatus::ReusingExisting;
            }
            Ok(_) | Err(TrustError::Failed(_)) => {}
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

    /// Makes sure a certificate is ready for the guests and, when puddle's own is used, trusted.
    /// The system's confirmation may appear (once, the first time puddle's own leaf is trusted).
    /// A declined or failed trust is reported in the status, not as an error.
    ///
    /// # Errors
    ///
    /// The data folder can't be written or the leaf can't be generated.
    pub fn ensure(&self, now: SystemTime) -> Result<DevCertReport, DevCertError> {
        let mut skipped = Vec::new();
        let mut unsupported = false;
        let mut detail = None;
        match self.trust.existing() {
            Ok(existing) => {
                let ranking = rank(&existing, now);
                skipped = ranking.skipped;
                for cert in ranking.eligible {
                    match self.trust.export_key(&cert.der) {
                        Ok(key) => {
                            return Ok(self.finish(
                                DevCertStatus::ReusingExisting,
                                DevCertMaterial::new(Origin::ReusedAspNet, cert.der.clone(), key),
                                skipped,
                                None,
                            ));
                        }
                        Err(error) => skipped.push(SkippedExisting {
                            fingerprint: crate::Fingerprint::of(&cert.der),
                            marker_version: leaf::inspect(&cert.der)
                                .ok()
                                .and_then(|i| i.marker_version)
                                .unwrap_or_default(),
                            reason: SkipReason::ExportFailed(error.to_string()),
                        }),
                    }
                }
            }
            Err(TrustError::Unsupported) => unsupported = true,
            Err(TrustError::Failed(why)) => detail = Some(why),
        }

        let (cert_der, key) = self.leaf(now)?;
        let (status, why) = if unsupported {
            (DevCertStatus::Unsupported, None)
        } else {
            self.trust_leaf(&cert_der)
        };
        let detail = why.or(detail);
        Ok(self.finish(
            status,
            DevCertMaterial::new(Origin::Generated, cert_der, key),
            skipped,
            detail,
        ))
    }

    fn finish(
        &self,
        status: DevCertStatus,
        material: DevCertMaterial,
        skipped: Vec<SkippedExisting>,
        detail: Option<String>,
    ) -> DevCertReport {
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((status, detail.clone().unwrap_or_default()));
        DevCertReport {
            status,
            material,
            skipped,
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
        // Only a certificate of the shape puddle makes is ever offered to the system.
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
        let blocks = pem::parse_many(text.as_bytes()).ok()?;
        let cert = blocks.iter().find(|b| b.tag() == "CERTIFICATE")?;
        let key = blocks.iter().find(|b| b.tag() == "PRIVATE KEY")?;
        let info = check_trust_shape(cert.contents()).ok()?;
        let now = unix_secs(now);
        let renew = i64::try_from(RENEW_WITHIN.as_secs()).ok()?;
        let good = info.not_before <= now && info.not_after - now >= renew;
        good.then(|| {
            (
                cert.contents().to_vec(),
                PrivateKey::from_pkcs8_der(key.contents().to_vec()),
            )
        })
    }
}
