// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_sign_in_secrets_come_out_once_and_a_second_taker_is_told_why() {
    let clock = Arc::new(ManualClock::new(0)) as Arc<dyn Clock>;
    let store = Store::open_in_memory(clock, Limits::default()).unwrap();
    assert!(sign_in_secrets(&store).is_ok());
    let err = sign_in_secrets(&store).unwrap_err();
    assert!(err.contains("taken already"), "{err}");
}
