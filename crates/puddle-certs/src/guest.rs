// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the guest gets: the synced roots and Puddle's CAs as boot files, the step that builds
//! the full bundle in the guest, and the environment that points tools at it.

use std::collections::BTreeSet;

use puddle_ca::{CaCertificate, TrustBundle};
use puddle_types::{FileMode, GuestEnv, GuestFile, GuestPath, SessionUser, guest_path};

use crate::devcert::GuestDevCert;
use crate::select::{CorporateRoots, Fingerprint};

/// Directory for the synced host certificates, one `<fingerprint prefix>.crt` each, inside
/// `update-ca-certificates`' input so the system store gets them (Debian, Ubuntu, Alpine).
pub const HOST_CA_DIR: &str = "/usr/local/share/ca-certificates/puddle-host";

/// The mode of the step scripts: executable.
const STEP_MODE: FileMode = FileMode::new(0o755);

/// Every extra CA (synced host certificates, then Puddle's CAs), each once:
/// `NODE_EXTRA_CA_CERTS`, which adds to Node's built-in roots.
pub const EXTRA_CAS_PATH: &str = "/etc/puddle/extra-cas.pem";

/// The full bundle the step writes: the image's bundle plus [`EXTRA_CAS_PATH`], each
/// certificate once. `SSL_CERT_FILE`, `REQUESTS_CA_BUNDLE` and `CURL_CA_BUNDLE` point here.
pub const CA_BUNDLE_PATH: &str = "/etc/puddle/ca-bundle.pem";

/// The step script (`guest/ca-bundle.sh`) in the guest.
pub const BUNDLE_STEP_PATH: &str = "/usr/local/lib/puddle/ca-bundle.sh";

/// The step script's text.
pub const BUNDLE_STEP_SH: &str = include_str!("../guest/ca-bundle.sh");

/// The step script (`guest/java-trust.sh`) in the guest: imports the extra CAs into the JVM's
/// `cacerts` when the image has a JDK or JRE.
pub const JAVA_STEP_PATH: &str = "/usr/local/lib/puddle/java-trust.sh";

/// The Java step script's text.
pub const JAVA_STEP_SH: &str = include_str!("../guest/java-trust.sh");

/// Hex digits of the fingerprint in a host certificate's file name: enough to never collide in
/// one store set, short enough to read.
const NAME_HEX: usize = 16;

/// The guest side of root sync for one workspace: the host's synced roots plus Puddle's own CAs
/// ([`TrustBundle`], a list so a dev CA is one more entry), and the development certificate when
/// the host has one ([`GuestTrust::with_dev_cert`]).
///
/// Hand [`GuestTrust::guest_files`], [`GuestTrust::env`] and [`GuestTrust::boot_steps`] to the
/// boot plan. With nothing to add, all three are empty and the guest keeps the image's trust.
///
/// ```
/// use puddle_ca::TrustBundle;
/// use puddle_certs::{CA_BUNDLE_PATH, CorporateRoots, GuestTrust};
///
/// let trust = GuestTrust::new(&CorporateRoots::default(), &TrustBundle::new());
/// assert!(trust.guest_files().is_empty() && trust.env().is_empty() && trust.boot_steps().is_empty());
/// # let _ = CA_BUNDLE_PATH;
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestTrust {
    host: Vec<(Fingerprint, String)>,
    puddle: TrustBundle,
    dev: Option<GuestDevCert>,
    user: SessionUser,
}

impl GuestTrust {
    /// The trust for a workspace from the host's `corporate` roots and Puddle's CAs.
    #[must_use]
    pub fn new(corporate: &CorporateRoots, puddle: &TrustBundle) -> Self {
        Self {
            host: corporate
                .certificates()
                .iter()
                .map(|c| (c.fingerprint(), c.pem()))
                .collect(),
            puddle: puddle.clone(),
            dev: None,
            user: SessionUser::Root,
        }
    }

    /// The same trust for a guest whose sessions run as `user` (root when not set): .NET's trust
    /// folder is in that user's home.
    #[must_use]
    pub fn for_user(mut self, user: SessionUser) -> Self {
        self.user = user;
        self
    }

    /// The same trust with the development certificate: trusted by the guest's system store, by
    /// Node and by the bundle, with its key and the tool variables (see [`GuestDevCert`]).
    #[must_use]
    pub fn with_dev_cert(mut self, dev: GuestDevCert) -> Self {
        self.dev = Some(dev);
        self
    }

    /// Whether there is nothing to add to the guest.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.host.is_empty() && self.puddle.certificates().is_empty() && self.dev.is_none()
    }

    /// The boot files, in a fixed order (the same inputs give byte-identical files, so the boot
    /// hook sees no change and skips `update-ca-certificates`):
    ///
    /// 1. one file per synced host certificate under [`HOST_CA_DIR`];
    /// 2. Puddle's CA files ([`TrustBundle::guest_files`]);
    /// 3. the development certificate's system-store copy, if there is one;
    /// 4. [`EXTRA_CAS_PATH`];
    /// 5. the step scripts at [`BUNDLE_STEP_PATH`] and [`JAVA_STEP_PATH`] (mode `0755`);
    /// 6. the development certificate's own files, if there is one: certificate, key (mode
    ///    `0600`) and step script.
    ///
    /// Everything is a public certificate or a script, except the development key.
    #[must_use]
    pub fn guest_files(&self) -> Vec<GuestFile> {
        if self.is_empty() {
            return Vec::new();
        }
        let mut files: Vec<GuestFile> = self
            .host
            .iter()
            .filter_map(|(fingerprint, pem)| {
                let hex = fingerprint.to_string();
                let name = hex.get(..NAME_HEX)?;
                let path = GuestPath::new(&format!("{HOST_CA_DIR}/{name}.crt")).ok()?;
                Some(GuestFile::new(path, pem.clone().into_bytes()))
            })
            .collect();
        files.extend(self.puddle.guest_files());
        let dev = self.dev.as_ref().map(GuestDevCert::guest_files);
        // The first of the development files is its system-store copy; the rest follow the step.
        let (dev_system, dev_rest) = dev.map_or((None, Vec::new()), |mut all| {
            let rest = all.split_off(1);
            (all.pop(), rest)
        });
        files.extend(dev_system);
        files.push(GuestFile::new(
            guest_path!(EXTRA_CAS_PATH),
            self.extra_cas().into_bytes(),
        ));
        files.push(
            GuestFile::new(
                guest_path!(BUNDLE_STEP_PATH),
                BUNDLE_STEP_SH.as_bytes().to_vec(),
            )
            .with_mode(STEP_MODE),
        );
        files.push(
            GuestFile::new(
                guest_path!(JAVA_STEP_PATH),
                JAVA_STEP_SH.as_bytes().to_vec(),
            )
            .with_mode(STEP_MODE),
        );
        files.extend(dev_rest);
        files
    }

    /// `NODE_EXTRA_CA_CERTS` and `DENO_CERT` (the extra CAs only: Node and Deno keep their own
    /// roots and add these) and `SSL_CERT_FILE`, `REQUESTS_CA_BUNDLE`, `CURL_CA_BUNDLE`,
    /// `PIP_CERT`, `GIT_SSL_CAINFO` (the full bundle, for tools that replace their roots:
    /// OpenSSL, Python, requests, pip, curl, git). Java has no variable for this and gets the
    /// CAs in its `cacerts` from the [`JAVA_STEP_PATH`] step. With a development certificate also its file paths, .NET's trust
    /// folder and no generated .NET certificate. Empty when there is nothing to add.
    #[must_use]
    pub fn env(&self) -> GuestEnv {
        let mut env = GuestEnv::new();
        if self.is_empty() {
            return env;
        }
        for (name, value) in [
            ("NODE_EXTRA_CA_CERTS", EXTRA_CAS_PATH),
            ("DENO_CERT", EXTRA_CAS_PATH),
            ("SSL_CERT_FILE", CA_BUNDLE_PATH),
            ("REQUESTS_CA_BUNDLE", CA_BUNDLE_PATH),
            ("CURL_CA_BUNDLE", CA_BUNDLE_PATH),
            ("PIP_CERT", CA_BUNDLE_PATH),
            ("GIT_SSL_CAINFO", CA_BUNDLE_PATH),
        ] {
            // Fixed, valid names and values: can't fail.
            let _ = env.set(name, value);
        }
        if self.dev.is_some() {
            env.extend(&GuestDevCert::env(self.user));
        }
        env
    }

    /// The steps the boot plan runs at every boot, in order, after `update-ca-certificates`:
    /// the one that builds [`CA_BUNDLE_PATH`] from the image's bundle, the Java one, then (with a development
    /// certificate) the one that sets up .NET. Empty when there is nothing to add.
    #[must_use]
    pub fn boot_steps(&self) -> Vec<GuestPath> {
        if self.is_empty() {
            return Vec::new();
        }
        [guest_path!(BUNDLE_STEP_PATH), guest_path!(JAVA_STEP_PATH)]
            .into_iter()
            .chain(self.dev.as_ref().map(|_| GuestDevCert::step()))
            .collect()
    }

    /// The text of [`EXTRA_CAS_PATH`]: host certificates, then Puddle's CAs, then the development
    /// certificate, each once (a dev CA the user also added to their Windows store is not
    /// repeated).
    fn extra_cas(&self) -> String {
        let mut seen = BTreeSet::new();
        let host = self.host.iter().map(|(fp, pem)| (*fp, pem.clone()));
        let puddle = self
            .puddle
            .certificates()
            .iter()
            .map(|c: &CaCertificate| (Fingerprint::of(c.der()), c.pem().to_owned()));
        let dev = self.dev.iter().filter_map(|d| {
            Some((
                Fingerprint::of(&der_of(d.cert_pem())?),
                d.cert_pem().to_owned(),
            ))
        });
        host.chain(puddle)
            .chain(dev)
            .filter(|(fp, _)| seen.insert(*fp))
            .map(|(_, pem)| pem)
            .collect()
    }
}

/// The DER of a single-certificate PEM.
fn der_of(cert_pem: &str) -> Option<Vec<u8>> {
    pem::parse(cert_pem).ok().map(pem::Pem::into_contents)
}

#[cfg(test)]
mod tests;
