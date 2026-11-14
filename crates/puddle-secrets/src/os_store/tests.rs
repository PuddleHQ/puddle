// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::*;

static ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
static UP: AtomicBool = AtomicBool::new(false);

fn counted() -> Result<(), StoreError> {
    ATTEMPTS.fetch_add(1, Ordering::SeqCst);
    if UP.load(Ordering::SeqCst) {
        Ok(())
    } else {
        Err(StoreError)
    }
}

/// One test body: the two statics are shared, so the cases run in order.
#[test]
fn a_failed_connection_is_tried_again_and_a_working_one_is_kept() {
    let connection = Connection::new(counted);
    UP.store(false, Ordering::SeqCst);

    assert_eq!(connection.ensure(), Err(StoreError));
    assert_eq!(connection.ensure(), Err(StoreError));
    assert_eq!(ATTEMPTS.load(Ordering::SeqCst), 2, "a failure is not kept");

    UP.store(true, Ordering::SeqCst);
    assert_eq!(connection.ensure(), Ok(()));
    assert_eq!(connection.ensure(), Ok(()));
    assert_eq!(ATTEMPTS.load(Ordering::SeqCst), 3, "a success is kept");

    assert_eq!(connection.reconnect(), Ok(()));
    assert_eq!(ATTEMPTS.load(Ordering::SeqCst), 4, "reconnect always tries");

    UP.store(false, Ordering::SeqCst);
    assert_eq!(connection.reconnect(), Err(StoreError));
    assert_eq!(
        connection.ensure(),
        Err(StoreError),
        "a failed reconnect forgets the earlier success"
    );
    assert_eq!(ATTEMPTS.load(Ordering::SeqCst), 6);
}
