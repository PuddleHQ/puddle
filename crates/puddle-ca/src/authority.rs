// SPDX-License-Identifier: AGPL-3.0-or-later
//! The per-workspace CA and its leaf cache.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, PublicKeyData, SanType, SignatureAlgorithm, SigningKey,
};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::sign::CertifiedKey;
use time::OffsetDateTime;
use zeroize::Zeroize as _;

use crate::name::DnsName;
use crate::{CaCertificate, CaError};

/// X.520's upper bound for a common name.
const MAX_COMMON_NAME: usize = 64;

/// Settings for a new CA.
///
/// The CA carries no name constraint: which hosts the proxy decrypts is decided by the
/// workspace's decrypt set and never by what the guest's trust would verify, so a host added to
/// the set while the workspace runs needs no new CA. It signs for DNS names only, never for an IP
/// address.
///
/// ```
/// use puddle_ca::CaBuilder;
///
/// let ca = CaBuilder::new("Puddle proxy CA (workspace demo)").build()?;
/// let leaf = ca.leaf("github.com")?;
/// assert_eq!(leaf.cert.len(), 1);
/// assert!(ca.leaf("example.com").is_ok());
/// assert!(ca.leaf("140.82.112.3").is_err());
/// # Ok::<(), puddle_ca::CaError>(())
/// ```
#[derive(Debug, Clone)]
pub struct CaBuilder {
    common_name: String,
    ca_validity: Duration,
    leaf_validity: Duration,
    backdate: Duration,
    leaf_cache_capacity: usize,
}

impl CaBuilder {
    /// Default CA lifetime: one year. The CA exists only in memory, so it is replaced at every
    /// start of Puddle long before that.
    pub const DEFAULT_CA_VALIDITY: Duration = Duration::from_hours(365 * 24);
    /// Default leaf lifetime: seven days. A cached leaf is replaced after three quarters of it.
    pub const DEFAULT_LEAF_VALIDITY: Duration = Duration::from_hours(7 * 24);
    /// Default backdating of `notBefore`: one day, for a guest clock that lags after the host
    /// slept.
    pub const DEFAULT_BACKDATE: Duration = Duration::from_hours(24);
    /// Default number of cached leaves per CA. The decrypt set can hold a pattern that covers
    /// many names, so the cache is bounded; the least recently used leaf goes first.
    pub const DEFAULT_LEAF_CACHE_CAPACITY: usize = 256;

    /// A CA named `common_name`, which is visible in the guest's trust store.
    #[must_use]
    pub fn new(common_name: &str) -> Self {
        Self {
            common_name: common_name.to_owned(),
            ca_validity: Self::DEFAULT_CA_VALIDITY,
            leaf_validity: Self::DEFAULT_LEAF_VALIDITY,
            backdate: Self::DEFAULT_BACKDATE,
            leaf_cache_capacity: Self::DEFAULT_LEAF_CACHE_CAPACITY,
        }
    }

    /// How long the CA certificate is valid from now.
    #[must_use]
    pub fn ca_validity(mut self, validity: Duration) -> Self {
        self.ca_validity = validity;
        self
    }

    /// How long each leaf is valid from its issue (never past the CA's end).
    #[must_use]
    pub fn leaf_validity(mut self, validity: Duration) -> Self {
        self.leaf_validity = validity;
        self
    }

    /// How far before now `notBefore` is set, on the CA and every leaf.
    #[must_use]
    pub fn backdate(mut self, backdate: Duration) -> Self {
        self.backdate = backdate;
        self
    }

    /// The most leaves kept in the cache.
    #[must_use]
    pub fn leaf_cache_capacity(mut self, capacity: usize) -> Self {
        self.leaf_cache_capacity = capacity;
        self
    }

    /// Generates the CA key (ECDSA P-256) and its self-signed certificate: `CA:TRUE` with a path
    /// length of 0 (it signs leaves, never another CA), critical, and no name constraint.
    ///
    /// # Errors
    ///
    /// [`CaError::InvalidSetting`] for an empty or over-long common name, a zero validity, a leaf
    /// validity longer than the CA's, or a zero cache capacity, and [`CaError::Generate`] when key
    /// generation or signing fails.
    pub fn build(self) -> Result<WorkspaceCa, CaError> {
        self.build_at(OffsetDateTime::now_utc())
    }

    pub(crate) fn build_at(self, now: OffsetDateTime) -> Result<WorkspaceCa, CaError> {
        let invalid = |setting, reason| Err(CaError::InvalidSetting { setting, reason });
        if self.common_name.trim().is_empty() || self.common_name.len() > MAX_COMMON_NAME {
            return invalid("common name", "must be 1 to 64 bytes");
        }
        if self.ca_validity.is_zero() || self.leaf_validity.is_zero() {
            return invalid("validity", "must not be zero");
        }
        if self.leaf_validity > self.ca_validity {
            return invalid("leaf validity", "must not exceed the CA validity");
        }
        if self.leaf_cache_capacity == 0 {
            return invalid("leaf cache capacity", "must be at least 1");
        }
        let (Some(not_before), Some(not_after)) = (
            checked_sub(now, self.backdate),
            checked_add(now, self.ca_validity),
        ) else {
            return invalid("validity", "out of range");
        };

        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, self.common_name.as_str());
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params.not_before = not_before;
        params.not_after = not_after;

        let key = WipedKey(KeyPair::generate().map_err(CaError::Generate)?);
        let cert = params.self_signed(&key).map_err(CaError::Generate)?;
        Ok(WorkspaceCa {
            certificate: CaCertificate::new(&cert),
            issuer: Issuer::new(params, key),
            not_before,
            not_after,
            leaf_validity: self.leaf_validity,
            backdate: self.backdate,
            cache: Mutex::new(LeafCache::new(self.leaf_cache_capacity)),
        })
    }
}

/// The CA's signing key, whose serialised form is overwritten when the CA drops. (The key inside
/// `ring` is not reachable and stays as it is.)
struct WipedKey(KeyPair);

impl PublicKeyData for WipedKey {
    fn der_bytes(&self) -> &[u8] {
        self.0.der_bytes()
    }

    fn algorithm(&self) -> &'static SignatureAlgorithm {
        self.0.algorithm()
    }
}

impl SigningKey for WipedKey {
    fn sign(&self, msg: &[u8]) -> Result<Vec<u8>, rcgen::Error> {
        self.0.sign(msg)
    }
}

impl Drop for WipedKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// One workspace's CA (never shared between workspaces), with the leaves it issued.
///
/// The key stays inside: the type has no accessor for it and implements neither `Clone` nor
/// serde's traits, and `Debug` prints only the expiry. The CA itself does not limit which names it
/// signs for (the caller's decrypt set does, see [`CaBuilder`]); a dev CA that must hand its key
/// to the guest would be a separate, name-constrained type.
pub struct WorkspaceCa {
    certificate: CaCertificate,
    issuer: Issuer<'static, WipedKey>,
    not_before: OffsetDateTime,
    not_after: OffsetDateTime,
    leaf_validity: Duration,
    backdate: Duration,
    cache: Mutex<LeafCache>,
}

impl fmt::Debug for WorkspaceCa {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkspaceCa")
            .field("not_after", &self.not_after)
            .finish_non_exhaustive()
    }
}

impl WorkspaceCa {
    /// The CA certificate, for the guest's trust bundle.
    #[must_use]
    pub fn certificate(&self) -> &CaCertificate {
        &self.certificate
    }

    /// A leaf certificate and signing key for `host` (a DNS name), from the cache or newly
    /// issued. The chain holds the leaf only; the guest has the CA as a trust anchor.
    ///
    /// Any DNS name gets a leaf: whether the proxy may decrypt `host` is the caller's decision
    /// and must be made before this is called.
    ///
    /// # Errors
    ///
    /// [`CaError::InvalidHost`] for anything but a plain DNS name (an IP address, a wildcard, a
    /// port), [`CaError::Expired`] once the CA has expired, and [`CaError::Generate`] /
    /// [`CaError::LoadKey`] when issuing fails.
    pub fn leaf(&self, host: &str) -> Result<Arc<CertifiedKey>, CaError> {
        self.leaf_at(host, OffsetDateTime::now_utc())
    }

    pub(crate) fn leaf_at(
        &self,
        host: &str,
        now: OffsetDateTime,
    ) -> Result<Arc<CertifiedKey>, CaError> {
        let parsed = DnsName::parse(host).ok_or_else(|| CaError::InvalidHost {
            host: host.to_owned(),
        })?;
        if now >= self.not_after {
            return Err(CaError::Expired);
        }
        // Issuing under the lock keeps two connections from minting the same leaf twice; one
        // ECDSA key and signature take well under a millisecond.
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(leaf) = cache.get(&parsed, now) {
            return Ok(leaf);
        }
        let (leaf, refresh_at) = self.issue(&parsed, now)?;
        cache.insert(parsed, Arc::clone(&leaf), refresh_at);
        Ok(leaf)
    }

    fn issue(
        &self,
        host: &DnsName,
        now: OffsetDateTime,
    ) -> Result<(Arc<CertifiedKey>, OffsetDateTime), CaError> {
        let not_before =
            checked_sub(now, self.backdate).map_or(self.not_before, |t| t.max(self.not_before));
        let not_after =
            checked_add(now, self.leaf_validity).map_or(self.not_after, |t| t.min(self.not_after));
        let refresh_at =
            checked_add(now, self.leaf_validity / 4 * 3).map_or(not_after, |t| t.min(not_after));

        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, host.to_string());
        params.subject_alt_names = vec![SanType::DnsName(
            host.as_str().try_into().map_err(CaError::Generate)?,
        )];
        params.is_ca = IsCa::ExplicitNoCa;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.use_authority_key_identifier_extension = true;
        params.not_before = not_before;
        params.not_after = not_after;

        let mut key = KeyPair::generate().map_err(CaError::Generate)?;
        let cert = params
            .signed_by(&key, &self.issuer)
            .map_err(CaError::Generate)?;
        // `CertifiedKey::from_der` would hand the key to rustls, which frees it as it is. The
        // copy made here and the key pair's own are overwritten as soon as `ring` has parsed it.
        let mut der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let signing = rustls::crypto::ring::sign::any_ecdsa_type(&der);
        der.zeroize();
        key.zeroize();
        let certified =
            CertifiedKey::new(vec![cert.der().clone()], signing.map_err(CaError::LoadKey)?);
        // No key-match check: the key was made a moment ago and the certificate signed for it.
        Ok((Arc::new(certified), refresh_at))
    }

    #[cfg(test)]
    pub(crate) fn cached_leaves(&self) -> usize {
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .len()
    }

    #[cfg(test)]
    pub(crate) fn key_der_for_test(&self) -> Vec<u8> {
        self.issuer.key().0.serialize_der()
    }
}

fn checked_add(t: OffsetDateTime, d: Duration) -> Option<OffsetDateTime> {
    t.checked_add(time::Duration::try_from(d).ok()?)
}

fn checked_sub(t: OffsetDateTime, d: Duration) -> Option<OffsetDateTime> {
    t.checked_sub(time::Duration::try_from(d).ok()?)
}

struct LeafCache {
    entries: HashMap<DnsName, CachedLeaf>,
    capacity: usize,
    clock: u64,
}

struct CachedLeaf {
    leaf: Arc<CertifiedKey>,
    refresh_at: OffsetDateTime,
    last_used: u64,
}

impl LeafCache {
    fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity,
            clock: 0,
        }
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn get(&mut self, host: &DnsName, now: OffsetDateTime) -> Option<Arc<CertifiedKey>> {
        let tick = self.tick();
        let entry = self.entries.get_mut(host)?;
        if now >= entry.refresh_at {
            return None;
        }
        entry.last_used = tick;
        Some(Arc::clone(&entry.leaf))
    }

    fn insert(&mut self, host: DnsName, leaf: Arc<CertifiedKey>, refresh_at: OffsetDateTime) {
        let last_used = self.tick();
        if !self.entries.contains_key(&host) && self.entries.len() >= self.capacity {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(host, _)| host.clone());
            if let Some(oldest) = oldest {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(
            host,
            CachedLeaf {
                leaf,
                refresh_at,
                last_used,
            },
        );
    }
}

#[cfg(test)]
mod tests;
