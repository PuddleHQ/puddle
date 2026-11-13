// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use std::error::Error as _;

#[test]
fn messages_name_the_endpoint_and_keep_the_cause_as_source() {
    let e = IpcError::Io {
        op: "bind",
        endpoint: PathBuf::from("/r/x.sock"),
        source: io::Error::from(io::ErrorKind::AddrInUse),
    };
    assert_eq!(e.to_string(), "could not bind /r/x.sock");
    assert!(e.source().is_some());

    let e = IpcError::NameTaken {
        endpoint: PathBuf::from(r"\\.\pipe\puddle-ab"),
    };
    assert!(e.to_string().contains(r"\\.\pipe\puddle-ab"));
    assert!(e.to_string().contains("squatting"));
}

#[test]
fn every_variant_has_a_lowercase_message_without_a_trailing_period() {
    let p = || PathBuf::from("/p");
    let all = [
        IpcError::NameTaken { endpoint: p() },
        IpcError::ForeignEndpoint {
            endpoint: p(),
            root: p(),
        },
        IpcError::PathTooLong {
            path: p(),
            len: 200,
            max: 107,
        },
        IpcError::Root {
            path: p(),
            source: io::Error::other("x"),
        },
        IpcError::Random(getrandom::Error::UNSUPPORTED),
        IpcError::NoRuntime,
        IpcError::NotFound { endpoint: p() },
        IpcError::AccessDenied { endpoint: p() },
        IpcError::WrongOwner { endpoint: p() },
        IpcError::Busy { endpoint: p() },
        IpcError::Closed { endpoint: p() },
        IpcError::Io {
            op: "bind",
            endpoint: p(),
            source: io::Error::other("x"),
        },
    ];
    for e in all {
        let m = e.to_string();
        assert!(m.chars().next().unwrap().is_lowercase(), "{m}");
        assert!(!m.ends_with('.'), "{m}");
    }
}
