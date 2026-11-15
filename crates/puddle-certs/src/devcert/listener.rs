// SPDX-License-Identifier: AGPL-3.0-or-later
//! The certificate of the API's network listener (remote serving, opt-in).
//!
//! A reverse proxy on another machine speaks TLS to Puddle. The certificate is made here the way
//! the development certificate is: one end-entity leaf (`CA:false`, server authentication) in
//! the owner-only data folder, one file holding certificate and key written in one step,
//! replaced under 30 days left, and no authority key anywhere. It names the listener's interface
//! address and the public host names, so a proxy that checks names can trust it, and a proxy
//! that pins the leaf can pin its SHA-256 ([`ListenerCert::fingerprint`]). Public certificates
//! stay the proxy's job: this one never faces a browser.

use std::fs;
use std::io;
use std::net::IpAddr;
use std::path::Path;
use std::time::SystemTime;

use rcgen::{
    CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};
use zeroize::{Zeroize as _, Zeroizing};

use super::leaf::{self, BACKDATE, RENEW_WITHIN, VALIDITY, unix_secs};
use super::manager::DevCertError;
use super::material::PrivateKey;
use crate::Fingerprint;

/// The folder under the data folder that holds the listener's leaf.
pub const FOLDER: &str = "remote-cert";

/// The leaf's file: certificate and private key as two PEM blocks.
pub const FILE: &str = "listener.pem";

/// The certificate alone, as PEM, for the operator to copy to the proxy (it holds no secret).
pub const CERT_FILE: &str = "listener.crt";

/// The certificate and key the listener presents.
#[derive(Debug)]
pub struct ListenerCert {
    cert_der: Vec<u8>,
    key: PrivateKey,
}

impl ListenerCert {
    /// The certificate, DER.
    #[must_use]
    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    /// The private key, PKCS#8 DER.
    #[must_use]
    pub fn key(&self) -> &PrivateKey {
        &self.key
    }

    /// The certificate as a `CERTIFICATE` PEM block, for a proxy to trust.
    #[must_use]
    pub fn cert_pem(&self) -> String {
        pem::encode(&pem::Pem::new("CERTIFICATE", self.cert_der.clone()))
    }

    /// The SHA-256 of the certificate, for a proxy to pin.
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::of(&self.cert_der)
    }
}

/// The leaf for these names under `data_dir`: the stored one when it is still good and names
/// exactly these, else a new one, written before it is returned.
///
/// # Errors
///
/// [`DevCertError`] when the folder cannot be written or the leaf cannot be made.
pub fn ensure(
    data_dir: &Path,
    ips: &[IpAddr],
    dns_names: &[String],
    now: SystemTime,
) -> Result<ListenerCert, DevCertError> {
    let dir = data_dir.join(FOLDER);
    let folder = |source| DevCertError::Folder {
        path: dir.clone(),
        source,
    };
    if let Some(found) = load(&dir, ips, dns_names, now) {
        puddle_fs::private::write_atomic(&dir.join(CERT_FILE), found.cert_pem().as_bytes())
            .map_err(folder)?;
        return Ok(found);
    }
    let (cert_der, key) =
        generate(ips, dns_names, now).map_err(|e| DevCertError::Generate(e.to_string()))?;
    puddle_fs::private::create_dir(&dir).map_err(folder)?;
    let crt = pem::encode(&pem::Pem::new("CERTIFICATE", cert_der.clone()));
    puddle_fs::private::write_atomic(&dir.join(CERT_FILE), crt.as_bytes()).map_err(folder)?;
    let mut body = Zeroizing::new(pem::encode(&pem::Pem::new("CERTIFICATE", cert_der.clone())));
    body.push_str(&key.to_pem());
    puddle_fs::private::write_atomic(&dir.join(FILE), body.as_bytes()).map_err(folder)?;
    Ok(ListenerCert { cert_der, key })
}

fn generate(
    ips: &[IpAddr],
    dns_names: &[String],
    now: SystemTime,
) -> Result<(Vec<u8>, PrivateKey), rcgen::Error> {
    let mut params = CertificateParams::default();
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, "Puddle remote listener");
    for name in dns_names {
        params
            .subject_alt_names
            .push(SanType::DnsName(name.clone().try_into()?));
    }
    params
        .subject_alt_names
        .extend(ips.iter().copied().map(SanType::IpAddress));
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.use_authority_key_identifier_extension = true;
    params.not_before = (now - BACKDATE).into();
    params.not_after = (now + VALIDITY).into();
    let key = KeyPair::generate()?;
    let cert = params.self_signed(&key)?;
    Ok((
        cert.der().to_vec(),
        PrivateKey::from_pkcs8_der(key.serialize_der()),
    ))
}

/// The stored leaf, if the file is owner-only, parses, is an end-entity certificate naming
/// exactly these addresses and names, and has at least [`RENEW_WITHIN`] left.
fn load(dir: &Path, ips: &[IpAddr], dns_names: &[String], now: SystemTime) -> Option<ListenerCert> {
    let file = fs::File::open(dir.join(FILE)).ok()?;
    // A key file others can read is a leaked key: replace it, never use it.
    puddle_fs::private::check(&file).ok()?;
    let text = Zeroizing::new(io::read_to_string(&file).ok()?);
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
    let (cert_der, key) = (cert?, key?);
    let info = leaf::inspect(&cert_der).ok()?;
    let mut want_dns: Vec<&str> = dns_names.iter().map(String::as_str).collect();
    let mut have_dns: Vec<&str> = info.dns_names.iter().map(String::as_str).collect();
    let mut want_ips = ips.to_vec();
    let mut have_ips = info.ip_addresses.clone();
    want_dns.sort_unstable();
    have_dns.sort_unstable();
    want_ips.sort_unstable();
    have_ips.sort_unstable();
    let now = unix_secs(now);
    let renew = i64::try_from(RENEW_WITHIN.as_secs()).ok()?;
    let good = !info.is_ca()
        && info.basic_constraints.is_some()
        && info.not_before <= now
        && info.not_after - now >= renew
        && want_dns == have_dns
        && want_ips == have_ips;
    good.then_some(ListenerCert { cert_der, key })
}

#[cfg(test)]
mod tests;
