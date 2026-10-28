// SPDX-License-Identifier: GPL-3.0-or-later
//! Where a captured connection came from, as the agent tells the proxy.
//!
//! A client in a container (Docker, Podman, a Kubernetes node) runs in its own network namespace
//! and image, so it does not inherit the workspace's CA settings or trust files: if the proxy
//! decrypted its connection, every tool in it would refuse Puddle's certificate. The agent sees the
//! difference (the connection's source address is not one of the guest's own) and says so with
//! [`ORIGIN_HEADER`]: [`CONTAINER`] on the `CONNECT`.
//!
//! The header only ever makes the proxy decrypt less: a marked connection is passed through unless
//! its host needs a credential added. A guest that sets it wrongly avoids Puddle's decryption for
//! its own connection, which it can already do by refusing the certificate, and gains no access:
//! the rules, the audit and every credential still apply.

/// The request header that says which kind of client opened the connection.
pub const ORIGIN_HEADER: &str = "x-puddle-origin";

/// The value of [`ORIGIN_HEADER`] for a client in a container's network namespace.
pub const CONTAINER: &str = "container";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_name_is_lower_case_for_the_proxy_comparison() {
        assert_eq!(ORIGIN_HEADER, ORIGIN_HEADER.to_ascii_lowercase());
        assert_eq!(CONTAINER, "container");
    }
}
