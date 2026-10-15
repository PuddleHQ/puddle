// SPDX-License-Identifier: GPL-3.0-or-later
//! Entry points for the cargo-fuzz targets: what a guest's first bytes reach before any rule is
//! asked (the request head and its target, host names, the agent's stream preamble). Each function takes arbitrary bytes, never panics, and panics only when an invariant
//! the proxy's refusals rest on does not hold, which is what a fuzz target is looking for.

use puddle_netpolicy::normalise_host;

use crate::http::{HeadError, MAX_HEAD, MAX_HEADERS, parse_target, read_head};

/// What the proxy made of a request head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadOutcome {
    /// Refused with `431` (too large).
    TooLarge,
    /// Refused with `400`, or the stream ended: nothing reached the rules.
    Refused,
    /// A well-formed head whose target the proxy would decide on.
    Target {
        /// The normalised host.
        host: String,
        /// The port.
        port: u16,
    },
    /// A well-formed head the proxy does not take as a proxy request (`400`).
    NotAProxyRequest,
}

/// Reads `bytes` as a guest's stream: the head, its target and the host's normal form.
///
/// # Panics
///
/// When a head the parser accepted breaks a limit or a character rule, or a target it accepted
/// has no host, a zero port, a userinfo part, or a host that does not survive normalising twice.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "a fuzz entry point that cannot start its runtime has nothing to report"
)]
pub fn head(bytes: &[u8]) -> HeadOutcome {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a current-thread runtime");
    let mut reader = bytes;
    let read = runtime.block_on(read_head(&mut reader));
    let parsed = match read {
        Ok(Ok(head)) => head,
        Ok(Err(HeadError::TooLarge)) => return HeadOutcome::TooLarge,
        Ok(Err(HeadError::Empty | HeadError::Bad(_))) | Err(_) => return HeadOutcome::Refused,
    };
    assert!(
        bytes.len() - reader.len() <= MAX_HEAD,
        "a head over the limit was accepted"
    );
    assert!(
        parsed.headers.len() <= MAX_HEADERS,
        "more header lines than the limit were accepted"
    );
    let clean = |s: &str| !s.bytes().any(|b| b == 0x7f || (b < 0x20 && b != b'\t'));
    assert!(
        clean(&parsed.method)
            && clean(&parsed.uri)
            && clean(&parsed.version)
            && parsed.headers.iter().all(|h| clean(h)),
        "a control character was accepted in a head"
    );
    target(&parsed.method, &parsed.uri)
}

/// Parses `method` and `uri` as a request target and normalises its host.
///
/// # Panics
///
/// As [`head`], for the target part.
#[must_use]
pub fn target(method: &str, uri: &str) -> HeadOutcome {
    let Ok(raw) = parse_target(method, uri) else {
        return HeadOutcome::NotAProxyRequest;
    };
    assert!(!raw.host.is_empty(), "an empty host was accepted");
    assert!(!raw.host.contains('@'), "userinfo was accepted in {uri:?}");
    assert_ne!(raw.port, 0, "port 0 was accepted");
    let Ok(normal) = normalise_host(&raw.host) else {
        return HeadOutcome::NotAProxyRequest;
    };
    let host = normal.host().to_string();
    // The normal form is its own normal form: a second pass must not move it.
    let again = normalise_host(&host).map(|t| t.host().to_string());
    assert_eq!(
        again.as_deref(),
        Ok(host.as_str()),
        "{host:?} is not stable"
    );
    HeadOutcome::Target {
        host,
        port: raw.port,
    }
}

/// Normalises `bytes` as a host name.
///
/// # Panics
///
/// When an accepted host has a character that is not printable ASCII or does not survive being
/// normalised again.
pub fn host(bytes: &[u8]) {
    let Ok(raw) = std::str::from_utf8(bytes) else {
        return;
    };
    if let Ok(target) = normalise_host(raw) {
        let host = target.host().to_string();
        assert!(
            host.bytes().all(|b| b.is_ascii_graphic()),
            "{host:?} holds a character that is not printable ASCII"
        );
        let again = normalise_host(&host).map(|t| t.host().to_string());
        assert_eq!(
            again.as_deref(),
            Ok(host.as_str()),
            "{host:?} is not stable"
        );
    }
}

/// Reads `bytes` as the first line of an agent stream.
///
/// # Panics
///
/// When a preamble is accepted that is not shaped `\0puddle-<kind>/<version>\n`, or a refusal
/// echoes a control character or more than 32 characters of the kind.
pub fn preamble(bytes: &[u8]) {
    use puddle_agent_proto::kind::{PreambleError, parse_preamble};
    match parse_preamble(bytes) {
        Ok(_) => assert!(bytes.starts_with(b"\0puddle-") && bytes.ends_with(b"\n")),
        Err(PreambleError::UnknownKind(name)) => {
            assert!(name.chars().count() <= 32);
            assert!(!name.chars().any(char::is_control));
        }
        Err(PreambleError::Malformed) => {}
    }
}
