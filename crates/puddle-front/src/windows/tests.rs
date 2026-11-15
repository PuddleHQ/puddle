// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_count_follows_the_guards_per_workspace() {
    let windows = Windows::default();
    let (a, b) = (
        WorkspaceId::new("a").unwrap(),
        WorkspaceId::new("b").unwrap(),
    );
    let one = windows.open(&a);
    let two = windows.open(&a);
    let other = windows.open(&b);
    assert_eq!((windows.connected(&a), windows.connected(&b)), (2, 1));
    drop(one);
    assert_eq!(windows.connected(&a), 1);
    drop((two, other));
    assert_eq!((windows.connected(&a), windows.connected(&b)), (0, 0));
}

#[tokio::test]
async fn a_watcher_sees_the_count_reach_zero() {
    let windows = Windows::default();
    let a = WorkspaceId::new("a").unwrap();
    let guard = windows.open(&a);
    let mut seen = windows.watch(&a);
    let wait = tokio::spawn(async move { seen.wait_for(|n| *n == 0).await.map(|n| *n) });
    drop(guard);
    assert_eq!(wait.await.unwrap().unwrap(), 0);
}
