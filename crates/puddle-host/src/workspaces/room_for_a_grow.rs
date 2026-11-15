// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn room_is_plenty_up_to_half_of_what_is_free_tight_up_to_all_of_it_and_too_big_past_it() {
    assert_eq!(room(0, 100), Room::Plenty);
    assert_eq!(room(50, 100), Room::Plenty);
    assert_eq!(room(51, 100), Room::Tight);
    assert_eq!(room(100, 100), Room::Tight);
    assert_eq!(room(101, 100), Room::TooBig);
    assert_eq!(room(1, 0), Room::TooBig);
}
