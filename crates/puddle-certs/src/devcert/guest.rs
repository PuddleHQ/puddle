// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a workspace gets of the development certificate: the certificate and its key as boot
//! files, the system-store copy, the .NET step and the variables that point tools at the files.
//!
//! Built into the guest's trust by [`GuestTrust::with_dev_cert`](crate::GuestTrust::with_dev_cert).

use std::fmt;

use puddle_types::{GuestEnv, GuestFile, GuestPath, SessionUser};
use zeroize::Zeroizing;

use super::DevCertMaterial;

/// The folder in the guest that holds the certificate and its key.
pub const DEV_CERT_DIR: &str = "/etc/puddle/dev-cert";

/// The certificate (PEM): `DEV_CERT_PEM`.
pub const DEV_CERT_PATH: &str = "/etc/puddle/dev-cert/localhost.crt";

/// The private key (PEM, mode `0600`): `DEV_KEY_PEM`.
pub const DEV_KEY_PATH: &str = "/etc/puddle/dev-cert/localhost.key";

/// The certificate as the system store reads it (`update-ca-certificates`): curl, git and every
/// OpenSSL client trust the certificate through it.
pub const DEV_SYSTEM_CA_PATH: &str = "/usr/local/share/ca-certificates/puddle-dev/localhost.crt";

/// The step script (`guest/dev-cert.sh`) in the guest.
pub const DEV_STEP_PATH: &str = "/usr/local/lib/puddle/dev-cert.sh";

/// The step script's text.
pub const DEV_STEP_SH: &str = include_str!("../../guest/dev-cert.sh");

/// `SSL_CERT_DIR` for a guest whose sessions run as `user`: .NET's own trust folder in the user's
/// home first (where `dotnet dev-certs https --trust` writes), then the system's hashed
/// certificates. Without it .NET reports the certificate as untrusted although every OpenSSL
/// client accepts it.
///
/// ```
/// use puddle_certs::devcert::dotnet_ssl_cert_dir;
/// use puddle_types::SessionUser;
/// assert_eq!(
///     dotnet_ssl_cert_dir(SessionUser::Agent),
///     "/home/agent/.aspnet/dev-certs/trust:/etc/ssl/certs:/usr/lib/ssl/certs"
/// );
/// ```
#[must_use]
pub fn dotnet_ssl_cert_dir(user: SessionUser) -> String {
    format!(
        "{}/.aspnet/dev-certs/trust:/etc/ssl/certs:/usr/lib/ssl/certs",
        user.home()
    )
}

/// The development certificate and its key, ready for one workspace's boot plan.
///
/// The key is the leaf's own (never a certificate authority's). It is wiped from memory on drop,
/// and `Debug` shows no key bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct GuestDevCert {
    cert_pem: String,
    key_pem: Zeroizing<String>,
}

impl fmt::Debug for GuestDevCert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuestDevCert").finish_non_exhaustive()
    }
}

impl GuestDevCert {
    /// The certificate and key of `material`, as the guest will hold them.
    #[must_use]
    pub fn new(material: &DevCertMaterial) -> Self {
        Self {
            cert_pem: material.cert_pem(),
            key_pem: material.key().to_pem(),
        }
    }

    /// The certificate (PEM), a public file: the system store and the extra CAs take it.
    pub(crate) fn cert_pem(&self) -> &str {
        &self.cert_pem
    }

    /// The boot files, in a fixed order (the same material gives byte-identical files):
    /// the system-store copy, the certificate and the key under [`DEV_CERT_DIR`] (the key
    /// mode `0600`, so its bytes never print), then the step script (`0755`).
    pub(crate) fn guest_files(&self) -> Vec<GuestFile> {
        // The modes are constants within 0o7777: `with_mode` can't fail, and a file that somehow
        // could not be made would be left out, never written with the default mode.
        let file = |path: &str, contents: Vec<u8>, mode: u32| {
            GuestFile::new(guest_path(path), contents)
                .with_mode(mode)
                .ok()
        };
        [
            file(
                DEV_SYSTEM_CA_PATH,
                self.cert_pem.clone().into_bytes(),
                0o644,
            ),
            file(DEV_CERT_PATH, self.cert_pem.clone().into_bytes(), 0o644),
            file(DEV_KEY_PATH, self.key_pem.as_bytes().to_vec(), 0o600),
            file(DEV_STEP_PATH, DEV_STEP_SH.as_bytes().to_vec(), 0o755),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// The variables: the file paths for tools that take them (Vite, Angular, Next.js), .NET's
    /// trust folder in `user`'s home, and no generated .NET certificate of its own.
    pub(crate) fn env(user: SessionUser) -> GuestEnv {
        let mut env = GuestEnv::new();
        let cert_dir = dotnet_ssl_cert_dir(user);
        for (name, value) in [
            ("DEV_CERT_PEM", DEV_CERT_PATH),
            ("DEV_KEY_PEM", DEV_KEY_PATH),
            ("SSL_CERT_DIR", cert_dir.as_str()),
            ("DOTNET_GENERATE_ASPNET_CERTIFICATE", "false"),
        ] {
            // Fixed, valid names and values: can't fail.
            let _ = env.set(name, value);
        }
        env
    }

    /// The step the plan runs after the bundle step.
    pub(crate) fn step() -> GuestPath {
        guest_path(DEV_STEP_PATH)
    }
}

#[expect(
    clippy::expect_used,
    reason = "invariant: only called with this module's absolute, normalised constants"
)]
fn guest_path(p: &str) -> GuestPath {
    GuestPath::new(p).expect("a constant guest path is valid")
}
