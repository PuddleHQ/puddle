// SPDX-License-Identifier: GPL-3.0-or-later
//! The badge: how many requests wait for the user, and how that shows on the tray icon, the
//! taskbar and the window title.

use std::collections::BTreeSet;

use puddle_types::Event;

/// The open requests, kept from the event stream so the badge needs no query per event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenRequests {
    ids: BTreeSet<i64>,
}

impl OpenRequests {
    /// The requests that are open now (read from the store at start-up and after the event
    /// stream lost events).
    #[must_use]
    pub fn of(ids: impl IntoIterator<Item = i64>) -> Self {
        Self {
            ids: ids.into_iter().collect(),
        }
    }

    /// Follows an event. Returns whether the count changed.
    pub fn apply(&mut self, event: &Event) -> bool {
        match event {
            Event::PendingOpened { request } => self.ids.insert(request.id),
            Event::PendingClosed { id, .. } => self.ids.remove(id),
            _ => false,
        }
    }

    /// How many requests wait.
    #[must_use]
    pub fn count(&self) -> usize {
        self.ids.len()
    }
}

/// `3` as the title shows it; nothing waiting shows no number.
fn count_text(count: usize) -> String {
    if count > 99 {
        "99+".to_owned()
    } else {
        count.to_string()
    }
}

/// The window title: `Puddle (3)` while 3 requests wait and the badge is on, else `Puddle`.
#[must_use]
pub fn window_title(count: usize, on: bool) -> String {
    if on && count > 0 {
        format!("Puddle ({})", count_text(count))
    } else {
        "Puddle".to_owned()
    }
}

/// The tray icon's tooltip, and the line the tray menu shows above Open and Quit. This one is
/// always in words, with or without the badge, so the menu says what waits.
#[must_use]
pub fn waiting_line(count: usize) -> String {
    match count {
        0 => "Nothing is waiting".to_owned(),
        1 => "1 request is waiting".to_owned(),
        n => format!("{n} requests are waiting"),
    }
}

/// The tray tooltip: the app's name, and with the badge on, what waits.
#[must_use]
pub fn tooltip(count: usize, on: bool) -> String {
    if on && count > 0 {
        format!("Puddle: {}", waiting_line(count))
    } else {
        "Puddle".to_owned()
    }
}

/// A square RGBA image the badge dot is drawn on or is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pixels {
    /// Width and height in pixels.
    pub size: u32,
    /// Four bytes per pixel: red, green, blue, alpha.
    pub rgba: Vec<u8>,
}

/// The dot's colour: a red that has contrast against both light and dark taskbars.
const DOT: [u8; 4] = [0xd9, 0x2d, 0x20, 0xff];
/// The ring around the dot, so it stands out from an icon of a similar colour.
const RING: [u8; 4] = [0xff, 0xff, 0xff, 0xff];

/// Paints a dot of `radius` pixels with a ring around it, centred at `(cx, cy)`.
fn paint_dot(pixels: &mut Pixels, cx: f32, cy: f32, radius: f32) {
    let size = pixels.size;
    for y in 0..size {
        for x in 0..size {
            #[expect(clippy::cast_precision_loss, reason = "pixel coordinates are tiny")]
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let distance = (dx * dx + dy * dy).sqrt();
            let colour = if distance <= radius {
                DOT
            } else if distance <= radius + (radius / 4.0).max(1.0) {
                RING
            } else {
                continue;
            };
            let at = ((y * size + x) * 4) as usize;
            if let Some(pixel) = pixels.rgba.get_mut(at..at + 4) {
                pixel.copy_from_slice(&colour);
            }
        }
    }
}

/// `base` with the badge dot in its lower right corner (the tray icon while requests wait).
/// `None` when `base` is not a square of `size` pixels with four bytes each.
#[must_use]
pub fn with_dot(base: &Pixels) -> Option<Pixels> {
    let expected = (base.size as usize)
        .checked_mul(base.size as usize)?
        .checked_mul(4)?;
    if base.size == 0 || base.rgba.len() != expected {
        return None;
    }
    let mut out = base.clone();
    #[expect(clippy::cast_precision_loss, reason = "icon sizes are tiny")]
    let size = base.size as f32;
    paint_dot(&mut out, size * 0.72, size * 0.72, size * 0.2);
    Some(out)
}

/// The digits of the overlay's number, five rows of three pixels each (bit 2 is the left pixel).
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b001, 0b001, 0b001],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

/// The plus after a nine, three rows of three pixels.
const PLUS: [u8; 3] = [0b010, 0b111, 0b010];

/// The size of the taskbar overlay, in pixels.
const OVERLAY: u32 = 16;

/// Paints `rows` (three pixels wide) with `scale` pixels per cell, its top left corner at
/// `(left, top)`, in white.
fn paint_glyph(pixels: &mut Pixels, rows: &[u8], scale: u32, left: u32, top: u32) {
    for (row, bits) in (0..).zip(rows) {
        for column in 0..3_u32 {
            if bits >> (2 - column) & 1 == 0 {
                continue;
            }
            for dy in 0..scale {
                for dx in 0..scale {
                    let (x, y) = (left + column * scale + dx, top + row * scale + dy);
                    let at = ((y * pixels.size + x) * 4) as usize;
                    if let Some(pixel) = pixels.rgba.get_mut(at..at + 4) {
                        pixel.copy_from_slice(&RING);
                    }
                }
            }
        }
    }
}

/// The Windows taskbar's overlay: the number of waiting requests in white on the dot, 16 pixels,
/// on a transparent background. One to nine shows the digit, more shows `9+`. With nothing
/// waiting it is the plain dot (the shell shows no overlay then).
#[must_use]
pub fn overlay_number(count: usize) -> Pixels {
    let mut pixels = Pixels {
        size: OVERLAY,
        rgba: vec![0; (OVERLAY * OVERLAY * 4) as usize],
    };
    paint_dot(&mut pixels, 8.0, 8.0, 7.0);
    // One to nine is the digit; ten and more is the largest digit and a plus.
    let digit = DIGITS.get(count.min(9));
    match (count, digit) {
        (0, _) | (_, None) => {}
        (1..=9, Some(rows)) => paint_glyph(&mut pixels, rows, 2, 5, 3),
        (_, Some(nine)) => {
            paint_glyph(&mut pixels, nine, 2, 2, 3);
            paint_glyph(&mut pixels, &PLUS, 1, 10, 5);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use puddle_types::{PendingEnd, PendingSummary, WorkspaceName};

    use super::*;

    fn opened(id: i64) -> Event {
        Event::PendingOpened {
            request: PendingSummary {
                id,
                workspace: WorkspaceName::new("demo").unwrap(),
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
            workspace: WorkspaceName::new("demo").unwrap(),
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
}
