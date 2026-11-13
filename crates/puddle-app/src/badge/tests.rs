// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_types::{PendingEnd, PendingSummary, WorkspaceId};

use super::*;

fn opened(id: i64) -> Event {
    Event::PendingOpened {
        request: PendingSummary {
            id,
            workspace: WorkspaceId::new("demo").unwrap(),
            host: "example.org".to_owned(),
            registrable_domain: "example.org".to_owned(),
            port: 443,
            first_seen: 1,
            last_seen: 1,
            attempts: 1,
        },
    }
}

fn closed(id: i64) -> Event {
    Event::PendingClosed {
        workspace: WorkspaceId::new("demo").unwrap(),
        id,
        state: PendingEnd::Allowed,
        rule_id: Some(1),
    }
}

#[test]
fn the_count_follows_opened_and_closed_requests_once_each() {
    let mut open = OpenRequests::of([7]);
    assert_eq!(open.count(), 1);
    assert!(open.apply(&opened(8)));
    assert!(
        !open.apply(&opened(8)),
        "the same request twice counts once"
    );
    assert_eq!(open.count(), 2);
    assert!(open.apply(&closed(7)));
    assert!(!open.apply(&closed(7)), "closing it again changes nothing");
    assert!(!open.apply(&Event::RulesChanged {}));
    assert_eq!(open.count(), 1);
}

#[test]
fn the_title_and_tooltip_show_the_count_only_with_the_badge_on_and_something_waiting() {
    assert_eq!(window_title(3, true), "Puddle (3)");
    assert_eq!(window_title(0, true), "Puddle");
    assert_eq!(window_title(3, false), "Puddle");
    assert_eq!(window_title(250, true), "Puddle (99+)");
    assert_eq!(tooltip(1, true), "Puddle: 1 request is waiting");
    assert_eq!(tooltip(2, true), "Puddle: 2 requests are waiting");
    assert_eq!(tooltip(2, false), "Puddle");
    assert_eq!(tooltip(0, true), "Puddle");
}

#[test]
fn the_menu_line_always_says_what_waits() {
    assert_eq!(waiting_line(0), "Nothing is waiting");
    assert_eq!(waiting_line(1), "1 request is waiting");
    assert_eq!(waiting_line(12), "12 requests are waiting");
}

#[test]
fn the_dot_is_drawn_in_the_lower_right_and_leaves_the_rest_of_the_icon_alone() {
    let size = 32;
    let base = Pixels {
        size,
        rgba: vec![0x10; (size * size * 4) as usize],
    };
    let marked = with_dot(&base).unwrap();
    let pixel = |p: &Pixels, x: u32, y: u32| {
        let at = ((y * p.size + x) * 4) as usize;
        p.rgba[at..at + 4].to_vec()
    };
    assert_eq!(pixel(&marked, 23, 23), DOT.to_vec());
    assert_eq!(pixel(&marked, 2, 2), pixel(&base, 2, 2));
    assert_eq!(pixel(&marked, 2, 29), pixel(&base, 2, 29));
    assert_ne!(marked, base);
}

#[test]
fn a_base_that_is_not_a_square_of_four_byte_pixels_gets_no_dot() {
    assert!(
        with_dot(&Pixels {
            size: 4,
            rgba: vec![0; 10]
        })
        .is_none()
    );
    assert!(
        with_dot(&Pixels {
            size: 0,
            rgba: vec![]
        })
        .is_none()
    );
}

/// The digits as a reader draws them, `#` for a white cell.
const ART: [[&str; 5]; 10] = [
    ["###", "#.#", "#.#", "#.#", "###"],
    [".#.", "##.", ".#.", ".#.", "###"],
    ["###", "..#", "###", "#..", "###"],
    ["###", "..#", "###", "..#", "###"],
    ["#.#", "#.#", "###", "..#", "..#"],
    ["###", "#..", "###", "..#", "###"],
    ["###", "#..", "###", "#.#", "###"],
    ["###", "..#", "..#", "..#", "..#"],
    ["###", "#.#", "###", "#.#", "###"],
    ["###", "#.#", "###", "..#", "###"],
];
const ART_PLUS: [&str; 3] = [".#.", "###", ".#."];

fn pixel(p: &Pixels, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * p.size + x) * 4) as usize;
    [p.rgba[at], p.rgba[at + 1], p.rgba[at + 2], p.rgba[at + 3]]
}

/// Whether `art` drawn at `scale` from `(left, top)` has a white cell on pixel `(x, y)`.
fn white(art: &[&str], scale: u32, left: u32, top: u32, x: u32, y: u32) -> bool {
    let (Some(dx), Some(dy)) = (x.checked_sub(left), y.checked_sub(top)) else {
        return false;
    };
    let (column, row) = ((dx / scale) as usize, (dy / scale) as usize);
    art.get(row)
        .and_then(|line| line.as_bytes().get(column))
        .is_some_and(|cell| *cell == b'#')
}

#[test]
fn each_digit_is_drawn_in_white_on_the_dot() {
    for (count, art) in ART.iter().enumerate().skip(1).take(9) {
        let overlay = overlay_number(count);
        assert_eq!((overlay.size, overlay.rgba.len()), (16, 16 * 16 * 4));
        for y in 3..13 {
            for x in 5..11 {
                let expected = if white(art, 2, 5, 3, x, y) { RING } else { DOT };
                assert_eq!(pixel(&overlay, x, y), expected, "digit {count} at {x},{y}");
            }
        }
    }
}

#[test]
fn more_than_nine_shows_a_nine_and_a_plus() {
    for count in [10, 11, 99, 1000] {
        let overlay = overlay_number(count);
        for y in 3..13 {
            for x in 2..13 {
                let on = white(&ART[9], 2, 2, 3, x, y) || white(&ART_PLUS, 1, 10, 5, x, y);
                let drawn = pixel(&overlay, x, y);
                if on {
                    assert_eq!(drawn, RING, "9+ for {count} at {x},{y}");
                } else {
                    // The dot's red, or clear in this box's corners, which lie outside the dot.
                    assert!(drawn == DOT || drawn[3] == 0, "9+ for {count} at {x},{y}");
                }
            }
        }
    }
    assert_ne!(overlay_number(9), overlay_number(10));
    assert_eq!(overlay_number(10), overlay_number(500));
}

#[test]
fn the_overlay_is_a_round_dot_on_a_transparent_square() {
    for count in [0, 1, 9, 10] {
        let overlay = overlay_number(count);
        assert_eq!(pixel(&overlay, 0, 0), [0, 0, 0, 0], "{count}");
        assert_eq!(pixel(&overlay, 15, 15), [0, 0, 0, 0], "{count}");
        // The dot's rim, inside the ring, is its red.
        assert_eq!(pixel(&overlay, 8, 1), DOT, "{count}");
    }
    assert_eq!(
        pixel(&overlay_number(0), 8, 8),
        DOT,
        "no number with nothing waiting"
    );
}
