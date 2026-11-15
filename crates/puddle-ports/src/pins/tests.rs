// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn pin(guest_port: u16, host_port: u16) -> Pin {
    Pin {
        guest_port,
        host_port,
    }
}

#[test]
fn memory_pins_keep_replace_list_and_remove_per_workspace() {
    let pins = MemoryPins::default();
    let (a, b) = (
        WorkspaceId::new("a").unwrap(),
        WorkspaceId::new("b").unwrap(),
    );
    pins.pin(&a, pin(80, 8080)).unwrap();
    pins.pin(&a, pin(22, 2222)).unwrap();
    pins.pin(&b, pin(80, 8081)).unwrap();
    pins.pin(&a, pin(80, 9090)).unwrap();
    assert_eq!(pins.pinned(&a).unwrap(), [pin(22, 2222), pin(80, 9090)]);
    assert_eq!(pins.all().unwrap().len(), 3);
    assert!(pins.unpin(&a, 80).unwrap());
    assert!(!pins.unpin(&a, 80).unwrap());
    assert_eq!(pins.pinned(&b).unwrap(), [pin(80, 8081)]);
}
