// SPDX-License-Identifier: AGPL-3.0-or-later
use super::cache_key;

#[test]
fn the_cache_key_is_fixed_and_tells_the_parts_apart() {
    assert_eq!(
        cache_key(&["1", "img", "nft", "script"]),
        0x5669_5061_04ff_b5b5
    );
    assert_ne!(cache_key(&["ab", "c"]), cache_key(&["a", "bc"]));
}
