// SPDX-License-Identifier: AGPL-3.0-or-later
//! Test support: keys made from a fixed seed, and the bytes an SSH server, an SSH client and an
//! agent client send, so the filter and the observer are tested without a real `ssh`.

use signature::Signer;
use ssh_key::private::{Ed25519Keypair, PrivateKey};
use ssh_key::public::KeyData;

/// An Ed25519 key made from a one-byte seed (the same seed, the same key).
pub struct TestKey(PrivateKey);

impl TestKey {
    /// The key for `seed`.
    #[must_use]
    pub fn ed25519(seed: u8) -> Self {
        Self(PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32])))
    }

    /// The public key in wire format.
    ///
    /// # Panics
    ///
    /// Never for an Ed25519 key.
    #[must_use]
    pub fn blob(&self) -> Vec<u8> {
        self.0
            .public_key()
            .to_bytes()
            .unwrap_or_else(|_| unreachable!("an Ed25519 key encodes"))
    }

    /// The public key, parsed.
    #[must_use]
    pub fn key_data(&self) -> KeyData {
        self.0.public_key().key_data().clone()
    }

    /// The SHA-256 fingerprint, as `ssh-keygen -l` prints it.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        self.0
            .public_key()
            .fingerprint(ssh_key::HashAlg::Sha256)
            .to_string()
    }

    /// A signature blob over `data` (algorithm name, then the signature bytes).
    ///
    /// # Panics
    ///
    /// Never for an Ed25519 key.
    #[must_use]
    pub fn sign(&self, data: &[u8]) -> Vec<u8> {
        let signature: ssh_key::Signature = Signer::sign(&self.0, data);
        Vec::try_from(signature).unwrap_or_else(|_| unreachable!("a signature encodes"))
    }
}

/// `bytes` as an SSH string: a four-byte length, then the bytes.
#[must_use]
pub fn string(bytes: &[u8]) -> Vec<u8> {
    let mut out = len(bytes.len()).to_vec();
    out.extend_from_slice(bytes);
    out
}

fn len(n: usize) -> [u8; 4] {
    u32::try_from(n)
        .unwrap_or_else(|_| unreachable!("test data is small"))
        .to_be_bytes()
}

/// An agent message with its length in front.
#[must_use]
pub fn framed(message: &[u8]) -> Vec<u8> {
    string(message)
}

/// One unencrypted binary packet (RFC 4253 section 6) holding `payload`, with four bytes of padding.
#[must_use]
pub fn packet(payload: &[u8]) -> Vec<u8> {
    let mut body = vec![4];
    body.extend_from_slice(payload);
    body.extend_from_slice(&[0; 4]);
    string(&body)
}

/// What a server sends up to and with its `NEWKEYS`: the identification, a `KEXINIT`, the reply
/// (message 31) with `host` as host key and `signature` over the session, and `NEWKEYS`.
#[must_use]
pub fn server_kex(host: &TestKey, signature: &[u8]) -> Vec<u8> {
    let mut out = b"SSH-2.0-TestServer\r\n".to_vec();
    out.extend(packet(&[20; 17]));
    let mut reply = vec![31];
    reply.extend(string(&host.blob()));
    reply.extend(string(&[7; 32]));
    reply.extend(string(signature));
    out.extend(packet(&reply));
    out.extend(packet(&[21]));
    out
}

/// A `session-bind@openssh.com` request (unframed).
#[must_use]
pub fn bind(host_key: &[u8], session_id: &[u8], signature: &[u8], forwarding: bool) -> Vec<u8> {
    let mut out = vec![27];
    out.extend(string(b"session-bind@openssh.com"));
    out.extend(string(host_key));
    out.extend(string(session_id));
    out.extend(string(signature));
    out.push(u8::from(forwarding));
    out
}

/// The data a client signs to log in to `session_id` as `user` with `key`; with `host_key`, the
/// host-bound method that names the server's key.
#[must_use]
pub fn login(session_id: &[u8], key: &[u8], host_key: Option<&[u8]>) -> Vec<u8> {
    let mut out = string(session_id);
    out.push(50);
    out.extend(string(b"git"));
    out.extend(string(b"ssh-connection"));
    match host_key {
        Some(_) => out.extend(string(b"publickey-hostbound-v00@openssh.com")),
        None => out.extend(string(b"publickey")),
    }
    out.push(1);
    out.extend(string(b"ssh-ed25519"));
    out.extend(string(key));
    if let Some(host_key) = host_key {
        out.extend(string(host_key));
    }
    out
}

/// A sign request (unframed).
#[must_use]
pub fn sign_request(key: &[u8], data: &[u8], flags: u32) -> Vec<u8> {
    let mut out = vec![13];
    out.extend(string(key));
    out.extend(string(data));
    out.extend_from_slice(&flags.to_be_bytes());
    out
}
