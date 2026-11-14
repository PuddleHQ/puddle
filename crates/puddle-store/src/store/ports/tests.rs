// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use super::*;
use crate::clock::ManualClock;
use crate::store::Limits;

fn store() -> Store {
    let store = Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap();
    for name in ["blog", "shop"] {
        crate::testing::own(&store, &ws(name));
    }
    store
}

fn ws(text: &str) -> WorkspaceId {
    WorkspaceId::new(text).unwrap()
}

fn pin(guest_port: u16, host_port: u16) -> PinnedPort {
    PinnedPort {
        guest_port,
        host_port,
    }
}

#[test]
fn a_pin_is_kept_replaced_and_removed_per_workspace() {
    let store = store();
    let (shop, blog) = (ws("shop"), ws("blog"));
    store.pin_port(&shop, pin(8080, 18080)).unwrap();
    store.pin_port(&shop, pin(3000, 3000)).unwrap();
    store.pin_port(&blog, pin(3000, 3001)).unwrap();
    assert_eq!(
        store.pinned_ports(&shop).unwrap(),
        [pin(3000, 3000), pin(8080, 18080)]
    );
    // The same guest port pinned again moves to the new host port; it does not double.
    store.pin_port(&shop, pin(3000, 4000)).unwrap();
    assert_eq!(
        store.pinned_ports(&shop).unwrap(),
        [pin(3000, 4000), pin(8080, 18080)]
    );
    assert_eq!(store.pinned_ports(&blog).unwrap(), [pin(3000, 3001)]);
    assert_eq!(
        store.all_pinned_ports().unwrap(),
        [
            (blog.clone(), pin(3000, 3001)),
            (shop.clone(), pin(3000, 4000)),
            (shop.clone(), pin(8080, 18080)),
        ]
    );
    assert!(store.unpin_port(&shop, 3000).unwrap());
    assert!(!store.unpin_port(&shop, 3000).unwrap());
    assert_eq!(store.pinned_ports(&shop).unwrap(), [pin(8080, 18080)]);
}

#[test]
fn a_port_the_table_refuses_changes_nothing() {
    let store = store();
    let shop = ws("shop");
    store.pin_port(&shop, pin(3000, 3000)).unwrap();
    assert!(store.pin_port(&shop, pin(0, 3000)).is_err());
    assert!(store.pin_port(&shop, pin(3000, 0)).is_err());
    assert_eq!(store.pinned_ports(&shop).unwrap(), [pin(3000, 3000)]);
}

#[test]
fn a_row_that_is_not_a_port_or_a_workspace_is_reported_not_trusted() {
    let store = store();
    // Written behind the table's back: the checks refuse these, so go around them.
    {
        let conn = lock(&store.conn);
        conn.execute_batch(
            "PRAGMA ignore_check_constraints = ON;
                 INSERT INTO pinned_ports VALUES ('shop', 70000, 80);
                 INSERT INTO pinned_ports VALUES ('Not A Name', 80, 80);",
        )
        .unwrap();
    }
    assert!(matches!(
        store.pinned_ports(&ws("shop")),
        Err(StoreError::Corrupt {
            table: "pinned_ports",
            ..
        })
    ));
    assert!(matches!(
        store.all_pinned_ports(),
        Err(StoreError::Corrupt {
            table: "pinned_ports",
            ..
        })
    ));
}

#[test]
fn deleting_a_workspace_removes_its_pins_and_only_its_own() {
    let store = store();
    let (shop, blog) = (ws("shop"), ws("blog"));
    store.pin_port(&shop, pin(3000, 3000)).unwrap();
    store.pin_port(&blog, pin(3000, 3001)).unwrap();
    store.delete_workspace(&shop).unwrap();
    assert_eq!(store.pinned_ports(&shop).unwrap(), []);
    assert_eq!(store.pinned_ports(&blog).unwrap(), [pin(3000, 3001)]);
}
