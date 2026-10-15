// SPDX-License-Identifier: GPL-3.0-or-later
//! An NTLM client for the lab's NTLM proxy where there is no SSPI: it builds the Type 1 and Type 3
//! messages by hand, so the chain's three-leg `407` handling runs against a proxy that speaks
//! NTLM. The Type 3 is shaped (user, domain, NTLMv2-sized response) but proves no identity, which
//! is all the lab proxy checks.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]
#![allow(dead_code, reason = "each test binary uses some of the pieces")]

use puddle_upstream::{AuthError, Leg, Package, SecurityContext, TokenSource};

/// Opens [`HandBuiltNtlm`] contexts for `DOMAIN\user`.
#[derive(Debug)]
pub(crate) struct HandBuiltNtlmSource {
    pub(crate) domain: &'static str,
    pub(crate) user: &'static str,
}

impl TokenSource for HandBuiltNtlmSource {
    fn open(&self, _package: Package, _spn: &str) -> Result<Box<dyn SecurityContext>, AuthError> {
        Ok(Box::new(HandBuiltNtlm {
            domain: self.domain,
            user: self.user,
            sent_negotiate: false,
        }))
    }
}

#[derive(Debug)]
struct HandBuiltNtlm {
    domain: &'static str,
    user: &'static str,
    sent_negotiate: bool,
}

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// A security buffer: length, allocated length, offset.
fn buffer(len: usize, offset: usize) -> Vec<u8> {
    let len = u16::try_from(len).unwrap();
    let mut out = Vec::new();
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&u32::try_from(offset).unwrap().to_le_bytes());
    out
}

impl HandBuiltNtlm {
    fn negotiate() -> Vec<u8> {
        let mut msg = b"NTLMSSP\0".to_vec();
        msg.extend_from_slice(&1_u32.to_le_bytes());
        // UNICODE | NTLM | REQUEST_TARGET
        msg.extend_from_slice(&0x0000_0205_u32.to_le_bytes());
        msg.extend_from_slice(&[0; 16]); // empty domain and workstation buffers
        msg
    }

    fn authenticate(&self) -> Vec<u8> {
        let (domain, user, host) = (utf16(self.domain), utf16(self.user), utf16("LABHOST"));
        let nt = vec![7_u8; 220]; // NTLMv2-sized: the proxy reports "ntlmv2" past 200 bytes
        let mut offset = 64;
        let mut msg = b"NTLMSSP\0".to_vec();
        msg.extend_from_slice(&3_u32.to_le_bytes());
        msg.extend_from_slice(&buffer(0, offset)); // LM response
        msg.extend_from_slice(&buffer(nt.len(), offset)); // NT response
        offset += nt.len();
        msg.extend_from_slice(&buffer(domain.len(), offset));
        offset += domain.len();
        msg.extend_from_slice(&buffer(user.len(), offset));
        offset += user.len();
        msg.extend_from_slice(&buffer(host.len(), offset));
        msg.extend_from_slice(&buffer(0, 0)); // session key
        msg.extend_from_slice(&0x0000_0205_u32.to_le_bytes());
        assert_eq!(msg.len(), 64);
        msg.extend_from_slice(&nt);
        msg.extend_from_slice(&domain);
        msg.extend_from_slice(&user);
        msg.extend_from_slice(&host);
        msg
    }
}

impl SecurityContext for HandBuiltNtlm {
    fn step(&mut self, input: Option<&[u8]>) -> Result<Leg, AuthError> {
        if !self.sent_negotiate {
            self.sent_negotiate = true;
            return Ok(Leg {
                token: Self::negotiate(),
                complete: false,
            });
        }
        let challenge = input.unwrap_or_default();
        // A Type 2 message: signature, then the number 2.
        assert!(
            challenge.starts_with(b"NTLMSSP\0") && challenge.get(8..12) == Some(&[2, 0, 0, 0]),
            "the proxy's second leg must be an NTLM challenge: {challenge:?}"
        );
        Ok(Leg {
            token: self.authenticate(),
            complete: true,
        })
    }
}
