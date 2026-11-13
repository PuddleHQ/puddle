// SPDX-License-Identifier: AGPL-3.0-or-later
//! The host keys the Git hosts publish, as SHA-256 fingerprints.
//!
//! Puddle ends SSH to the Git hosts itself, so the filtered agent never signs a login to one: a
//! guest that reaches GitHub through a jump host, an address or another name would otherwise log in
//! as the user and skip the repository table. The list ships with Puddle and is renewed with each
//! release; it is never learned on first use and never taken from the guest. Azure DevOps publishes
//! only the fingerprint of its RSA key.
//!
//! Sources: GitHub's `https://api.github.com/meta` (`ssh_keys`), GitLab's "GitLab.com settings" page
//! (SSH `known_hosts` entries), Bitbucket's `https://bitbucket.org/site/ssh`, Microsoft's "Use SSH
//! key authentication" page for Azure DevOps.

use ssh_key::HashAlg;
use ssh_key::public::KeyData;

/// Every published key: GitHub, GitLab, Bitbucket, Azure DevOps.
const PUBLISHED: &[&str] = &[
    // GitHub: Ed25519, ECDSA, RSA.
    "SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU",
    "SHA256:p2QAMXNIC1TJYWeIOttrVc98/R1BUFWu3/LiyKgUfQM",
    "SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s",
    // GitLab.com: Ed25519, ECDSA, RSA.
    "SHA256:eUXGGm1YGsMAS7vkcx6JOJdOGHPem5gQp4taiCfCLB8",
    "SHA256:HbW3g8zUjNSksFbqTiUWPWg2Bq1x8xdGUrliXFzSnUw",
    "SHA256:ROQFvPThGrW4RuWLoL9tq9I9zJ42fK4XywyRtbOz/EQ",
    // Bitbucket Cloud: Ed25519, ECDSA, RSA.
    "SHA256:ybgmFkzwOSotHTHLJgHO0QN8L0xErw6vd0VhFA9m3SM",
    "SHA256:FC73VB6C4OQLSCrjEayhMp9UMxS97caD/Yyi2bhW/J0",
    "SHA256:46OSHA1Rmj8E8ERTC6xkNcmGOw9oFxYr0WF6zWW8l1E",
    // Azure DevOps: RSA.
    "SHA256:ohD8VZEXGWo6Ez8GSEJQ9WpafgLFsOfLOtGGQCQo6Og",
];

/// Whether `host_key` is one a Git host publishes.
///
/// The fingerprint is taken over the key as `ssh-key` encodes it, so a second encoding of the same
/// key on the wire (an RSA number with a leading zero byte, say) still matches.
#[must_use]
pub fn is_git_host_key(host_key: &KeyData) -> bool {
    let fingerprint = host_key.fingerprint(HashAlg::Sha256).to_string();
    PUBLISHED.contains(&fingerprint.as_str())
}

#[cfg(test)]
mod tests;
