// SPDX-License-Identifier: AGPL-3.0-or-later
//! A grow whose room cannot be measured goes ahead.

use super::*;

/// A drive whose free space cannot be read.
struct UnreadableDrive;

impl puddle_host::Space for UnreadableDrive {
    fn free_bytes(&self) -> std::io::Result<u64> {
        Err(std::io::Error::other("no such drive"))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_grow_goes_ahead_when_the_free_space_of_the_drive_cannot_be_read() {
    let rig = Rig::named_ids();
    *rig.free_space.lock().unwrap() = Some(Arc::new(UnreadableDrive));
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    // Larger than any free space a test machine has: with a readable drive it is refused.
    let end = grow_root_disk(&api, &mut events, u64::from(u32::MAX)).await;
    assert_eq!(end["step"], "done", "{end}");
    host.shutdown().await;
}
