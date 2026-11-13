// SPDX-License-Identifier: AGPL-3.0-or-later
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
mod tests;
