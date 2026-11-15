// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn wire<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn every_reason_code_is_its_wire_name_and_listed_once() {
    let mut seen = std::collections::HashSet::new();
    for code in &ReasonCode::ALL {
        assert_eq!(wire(code), code.as_str());
        assert_eq!(code.to_string(), code.as_str());
        assert!(seen.insert(*code), "{code} listed twice");
    }
}

#[test]
fn a_prefix_and_a_failed_request_code_are_their_wire_names() {
    for prefix in [
        ReasonPrefix::Toggle,
        ReasonPrefix::Limit,
        ReasonPrefix::PassThrough,
        ReasonPrefix::PuddleRequestFailed,
    ] {
        assert_eq!(wire(prefix), prefix.as_str());
    }
    for code in [
        FailedRequestCode::Unreachable,
        FailedRequestCode::Tls,
        FailedRequestCode::Timeout,
        FailedRequestCode::TooLarge,
        FailedRequestCode::Protocol,
        FailedRequestCode::BadToken,
        FailedRequestCode::HostRefused,
        FailedRequestCode::RateLimited,
        FailedRequestCode::HostError,
    ] {
        assert_eq!(wire(code), code.as_str());
        assert_eq!(code.to_string(), code.as_str());
    }
}
