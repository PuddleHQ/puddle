// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

#[test]
fn a_refused_spec_gives_its_reason_and_any_other_failure_its_message() {
    let refused = checkout_refusal(ComputeError::InvalidSpec {
        reason: "/work/x is owned by someone else".to_owned(),
    });
    assert_eq!(
        refused,
        LocalCheckoutError::Refused("/work/x is owned by someone else".to_owned())
    );
    let failed = checkout_refusal(ComputeError::Runtime {
        op: "check",
        message: "no space left on device".to_owned(),
    });
    assert_eq!(
        failed,
        LocalCheckoutError::Refused("runtime failed to check: no space left on device".to_owned())
    );
}
