// SPDX-License-Identifier: GPL-3.0-or-later
//! Puddle's localhost leaf certificate: making it, reading what a certificate says, and the
//! shape check that gates what is ever added to the Windows trust store.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::{Duration, SystemTime};

use rcgen::{
    CertificateParams, CustomExtension, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose, SanType,
};
use x509_parser::extensions::{GeneralName, ParsedExtension};
use x509_parser::prelude::{FromDer, X509Certificate};

use super::material::PrivateKey;

/// The names the leaf is valid for (HS-23): a leaked key impersonates only these.
pub const DNS_NAMES: [&str; 5] = [
    "localhost",
    "*.dev.localhost",
    "*.dev.internal",
    "host.docker.internal",
    "host.containers.internal",
];

/// The addresses the leaf is valid for.
pub const IP_ADDRESSES: [IpAddr; 2] = [
    IpAddr::V4(Ipv4Addr::LOCALHOST),
    IpAddr::V6(Ipv6Addr::LOCALHOST),
];

/// The ASP.NET development-certificate marker extension (`dotnet dev-certs` writes it, and
/// recognises a certificate by it).
pub const ASPNET_MARKER_OID: [u64; 10] = [1, 3, 6, 1, 4, 1, 311, 84, 1, 1];

/// The marker's value: .NET 10's certificate version, one raw byte.
pub const MARKER_VERSION: u8 = 6;

/// How long a new leaf is valid.
pub const VALIDITY: Duration = Duration::from_hours(365 * 24);

/// A leaf with less than this left is replaced, and an existing certificate with less than this
/// left is not reused.
pub const RENEW_WITHIN: Duration = Duration::from_hours(30 * 24);

/// A new leaf starts this long before now, so a host with a slow clock still accepts it.
const BACKDATE: Duration = Duration::from_hours(24);

/// A freshly generated leaf.
pub(crate) struct NewLeaf {
    pub(crate) cert_der: Vec<u8>,
    pub(crate) key: PrivateKey,
}

/// Why a leaf could not be made.
#[derive(Debug, thiserror::Error)]
#[error("generating the development certificate failed: {0}")]
pub(crate) struct GenerateError(String);

/// Makes a localhost leaf valid from a day before `now` for [`VALIDITY`]: a self-signed
/// end-entity certificate (`CA:false`, critical), `digitalSignature`, server authentication, the
/// names above and the ASP.NET marker. ECDSA P-256: the one key type ring generates.
pub(crate) fn generate(now: SystemTime) -> Result<NewLeaf, GenerateError> {
    let err = |e: rcgen::Error| GenerateError(e.to_string());
    let mut params = CertificateParams::default();
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, "localhost");
    for name in DNS_NAMES {
        let name = name.to_owned().try_into().map_err(err)?;
        params.subject_alt_names.push(SanType::DnsName(name));
    }
    params
        .subject_alt_names
        .extend(IP_ADDRESSES.into_iter().map(SanType::IpAddress));
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.use_authority_key_identifier_extension = true;
    params.custom_extensions = vec![CustomExtension::from_oid_content(
        &ASPNET_MARKER_OID,
        vec![MARKER_VERSION],
    )];
    params.not_before = (now - BACKDATE).into();
    params.not_after = (now + VALIDITY).into();

    let key = KeyPair::generate().map_err(err)?;
    let cert = params.self_signed(&key).map_err(err)?;
    Ok(NewLeaf {
        cert_der: cert.der().to_vec(),
        key: PrivateKey::from_pkcs8_der(key.serialize_der()),
    })
}

/// What a certificate says, for the choices Puddle makes about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertInfo {
    /// Start of validity, Unix seconds.
    pub not_before: i64,
    /// End of validity, Unix seconds.
    pub not_after: i64,
    /// Basic constraints: `Some((critical, ca))`, or `None` when the extension is absent.
    pub basic_constraints: Option<(bool, bool)>,
    /// DNS names in the subject alternative name.
    pub dns_names: Vec<String>,
    /// IP addresses in the subject alternative name.
    pub ip_addresses: Vec<IpAddr>,
    /// The ASP.NET marker's version byte, if the certificate carries the marker.
    pub marker_version: Option<u8>,
}

impl CertInfo {
    /// The certificate says it is a CA.
    #[must_use]
    pub fn is_ca(&self) -> bool {
        matches!(self.basic_constraints, Some((_, true)))
    }
}

/// `der` is not a certificate Puddle can read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a readable certificate")]
pub struct Unreadable;

/// Reads `der`.
///
/// # Errors
///
/// [`Unreadable`] if it is not one X.509 certificate.
pub fn inspect(der: &[u8]) -> Result<CertInfo, Unreadable> {
    let (rest, cert) = X509Certificate::from_der(der).map_err(|_| Unreadable)?;
    if !rest.is_empty() {
        return Err(Unreadable);
    }
    let mut info = CertInfo {
        not_before: cert.validity().not_before.timestamp(),
        not_after: cert.validity().not_after.timestamp(),
        basic_constraints: None,
        dns_names: Vec::new(),
        ip_addresses: Vec::new(),
        marker_version: None,
    };
    for ext in cert.extensions() {
        if ext
            .oid
            .iter()
            .is_some_and(|arcs| arcs.eq(ASPNET_MARKER_OID))
        {
            // `dotnet dev-certs` writes the version as one raw byte, not DER.
            if let [version] = ext.value {
                info.marker_version = Some(*version);
            }
            continue;
        }
        match ext.parsed_extension() {
            ParsedExtension::BasicConstraints(bc) => {
                info.basic_constraints = Some((ext.critical, bc.ca));
            }
            ParsedExtension::SubjectAlternativeName(san) => {
                for name in &san.general_names {
                    match name {
                        GeneralName::DNSName(dns) => info.dns_names.push((*dns).to_owned()),
                        GeneralName::IPAddress(ip) => {
                            if let Some(ip) = ip_from_bytes(ip) {
                                info.ip_addresses.push(ip);
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(info)
}

fn ip_from_bytes(bytes: &[u8]) -> Option<IpAddr> {
    if let Ok(octets) = <[u8; 4]>::try_from(bytes) {
        return Some(IpAddr::from(octets));
    }
    <[u8; 16]>::try_from(bytes).ok().map(IpAddr::from)
}

/// Seconds since the epoch (negative before it).
pub(crate) fn unix_secs(at: SystemTime) -> i64 {
    time::OffsetDateTime::from(at).unix_timestamp()
}

/// Why a certificate may not be added to the trust store.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ShapeError {
    /// Not a readable certificate.
    #[error("not a readable certificate")]
    Unreadable,
    /// It is a CA, or does not say it is not one.
    #[error("the certificate is, or may be, a certificate authority")]
    NotALeaf,
    /// It lacks the ASP.NET marker.
    #[error("the certificate has no ASP.NET development marker")]
    NoMarker,
    /// It names something outside the dev-certificate set, or names nothing.
    #[error("the certificate is valid for a name outside the localhost set: {0}")]
    ForeignName(String),
}

/// Whether the certificate is valid for loopback names only: at least one name, every one in
/// [`DNS_NAMES`] or [`IP_ADDRESSES`]. A key that can impersonate more than that must not be handed to
/// a workspace.
///
/// # Errors
///
/// [`ShapeError::ForeignName`] naming the first name outside the set, or `(none)`.
pub(crate) fn check_names(info: &CertInfo) -> Result<(), ShapeError> {
    if info.dns_names.is_empty() && info.ip_addresses.is_empty() {
        return Err(ShapeError::ForeignName("(none)".into()));
    }
    if let Some(name) = info
        .dns_names
        .iter()
        .find(|n| !DNS_NAMES.contains(&n.as_str()))
    {
        return Err(ShapeError::ForeignName(name.clone()));
    }
    if let Some(ip) = info
        .ip_addresses
        .iter()
        .find(|ip| !IP_ADDRESSES.contains(ip))
    {
        return Err(ShapeError::ForeignName(ip.to_string()));
    }
    Ok(())
}

/// Whether `der` has the shape Puddle ever trusts: a leaf (`CA:false`), carrying the marker, and
/// valid only for [`DNS_NAMES`] and [`IP_ADDRESSES`]. Trusting anything wider would let a leaked
/// key impersonate more than loopback.
///
/// # Errors
///
/// The first way it falls short.
pub fn check_trust_shape(der: &[u8]) -> Result<CertInfo, ShapeError> {
    let info = inspect(der).map_err(|Unreadable| ShapeError::Unreadable)?;
    if !matches!(info.basic_constraints, Some((_, false))) {
        return Err(ShapeError::NotALeaf);
    }
    if info.marker_version.is_none() {
        return Err(ShapeError::NoMarker);
    }
    check_names(&info)?;
    Ok(info)
}
