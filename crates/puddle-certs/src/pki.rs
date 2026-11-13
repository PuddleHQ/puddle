// SPDX-License-Identifier: AGPL-3.0-or-later
//! The plain-HTTP addresses a certificate names for fetching what a client needs to check a chain:
//! the CRL distribution points and the authority-information-access locations (the issuer's
//! certificate, the OCSP responder).

use x509_parser::extensions::{DistributionPointName, GeneralName, ParsedExtension};
use x509_parser::oid_registry::{
    OID_PKIX_ACCESS_DESCRIPTOR_CA_ISSUERS, OID_PKIX_ACCESS_DESCRIPTOR_OCSP,
};
use x509_parser::prelude::{FromDer, X509Certificate};

/// Every `http://` URL in the CRL distribution points and AIA extension of `der`, sorted and
/// without repeats. A certificate that cannot be read, or names none, gives none. Other schemes
/// (`ldap://`, `https://`) are left out: only plain HTTP is a destination of its own.
pub(crate) fn http_urls(der: &[u8]) -> Vec<String> {
    let Ok((_, cert)) = X509Certificate::from_der(der) else {
        return Vec::new();
    };
    let mut urls = Vec::new();
    let mut take = |name: &GeneralName<'_>| {
        if let GeneralName::URI(uri) = name
            && uri
                .get(..7)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("http://"))
        {
            urls.push((*uri).to_owned());
        }
    };
    for extension in cert.extensions() {
        match extension.parsed_extension() {
            ParsedExtension::CRLDistributionPoints(points) => {
                for point in &points.points {
                    if let Some(DistributionPointName::FullName(names)) = &point.distribution_point
                    {
                        names.iter().for_each(&mut take);
                    }
                }
            }
            ParsedExtension::AuthorityInfoAccess(access) => {
                for description in &access.accessdescs {
                    let method = &description.access_method;
                    if *method == OID_PKIX_ACCESS_DESCRIPTOR_CA_ISSUERS
                        || *method == OID_PKIX_ACCESS_DESCRIPTOR_OCSP
                    {
                        take(&description.access_location);
                    }
                }
            }
            _ => {}
        }
    }
    urls.sort_unstable();
    urls.dedup();
    urls
}

#[cfg(test)]
mod tests;
