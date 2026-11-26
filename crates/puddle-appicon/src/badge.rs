// SPDX-License-Identifier: AGPL-3.0-or-later
//! The badge drawn on Puddle's icon while requests wait: the number on a red dot for the Windows
//! taskbar button, and the dot in a corner of the tray icon.
//!
//! Everything is drawn from paths with anti-aliased coverage at the size the shell asks for
//! (the overlay is 16 pixels at 100 percent and grows with the display scale), so an edge is a
//! smooth alpha ramp and no pixel is blocky. [`overlay_side`] and [`tray_side`] give those sizes.

use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, PremultipliedColorU8, Stroke,
    Transform,
};

use crate::Icon;

/// The dot's colour: a red that has contrast against both light and dark taskbars.
pub const DOT: [u8; 3] = [0xd9, 0x2d, 0x20];
/// The number on the dot, and the ring round the tray's dot when there is one.
pub(crate) const WHITE: [u8; 3] = [0xff; 3];

/// The overlay's side at 100 percent display scale, as Windows draws it on the taskbar button.
const OVERLAY_BASE: f64 = 16.0;
/// The tray's side that serves every scale: 16, 20, 24 and 32 pixels are all within a gentle
/// downscale of it.
const TRAY_SIDE: u32 = 32;

/// The side of the taskbar overlay for a display `scale` (1.0 = 100 percent): 16, 20, 24 and
/// 32 pixels at 100, 125, 150 and 200 percent.
#[must_use]
pub fn overlay_side(scale: f64) -> u32 {
    let scale = if scale.is_finite() {
        scale.clamp(1.0, 4.0)
    } else {
        1.0
    };
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the scale is clamped to 1..4, so the side is 16..64"
    )]
    let side = (OVERLAY_BASE * scale).round() as u32;
    side
}

/// The side the tray icon is drawn at (Windows then shows it at 16 to 32 pixels).
#[must_use]
pub const fn tray_side() -> u32 {
    TRAY_SIDE
}

/// The taskbar overlay: the number of waiting requests in white on a red dot, `side` pixels square
/// on a transparent background. One to nine shows the digit, more shows `9+`. With nothing waiting
/// it is the plain dot (the shell shows no overlay then). `None` for a side of zero.
#[must_use]
pub fn overlay(count: usize, side: u32) -> Option<Icon> {
    let mut pixmap = Pixmap::new(side, side)?;
    let extent = f32_of(side);
    disc(&mut pixmap, extent / 2.0, extent / 2.0, extent / 2.0, DOT);
    match count {
        0 => {}
        1..=9 => digit(
            &mut pixmap,
            count,
            extent * 0.5,
            extent * 0.5,
            extent * 0.58,
        ),
        _ => {
            digit(&mut pixmap, 9, extent * 0.38, extent * 0.5, extent * 0.5);
            plus(
                &mut pixmap,
                extent * 0.72,
                extent * 0.5,
                extent * 0.3,
                extent,
            );
        }
    }
    Some(icon_of(&pixmap))
}

/// `base` with the red dot in its lower right corner (the tray icon while requests wait), with a
/// white ring round it when `ring` is set. `None` when `base` is not a square of `size` pixels
/// with four bytes each.
#[must_use]
pub fn with_dot(base: &Icon, ring: bool) -> Option<Icon> {
    let mut pixmap = pixmap_of(base)?;
    let size = f32_of(base.size);
    let (centre, radius) = (size * 0.72, size * 0.2);
    if ring {
        disc(
            &mut pixmap,
            centre,
            centre,
            radius + (radius / 4.0).max(1.0),
            WHITE,
        );
    }
    disc(&mut pixmap, centre, centre, radius, DOT);
    Some(icon_of(&pixmap))
}

impl Icon {
    /// The icon scaled to `side` pixels square with a Lanczos filter, colours weighted by their
    /// alpha so a soft edge does not pick up a dark fringe. `None` for a side of zero or a
    /// malformed icon.
    #[must_use]
    pub fn resized(&self, side: u32) -> Option<Self> {
        if side == 0 || self.size == 0 {
            return None;
        }
        if side == self.size {
            return Some(self.clone());
        }
        let mut weighted = image::RgbaImage::from_raw(self.size, self.size, self.rgba.clone())?;
        for pixel in weighted.pixels_mut() {
            let alpha = u32::from(pixel[3]);
            for channel in &mut pixel.0[..3] {
                *channel = u8::try_from((u32::from(*channel) * alpha + 127) / 255).unwrap_or(255);
            }
        }
        let mut out =
            image::imageops::resize(&weighted, side, side, image::imageops::FilterType::Lanczos3);
        for pixel in out.pixels_mut() {
            let alpha = u32::from(pixel[3]);
            for channel in &mut pixel.0[..3] {
                let straight = (u32::from(*channel) * 255 + alpha / 2)
                    .checked_div(alpha)
                    .unwrap_or(0);
                *channel = u8::try_from(straight).unwrap_or(255);
            }
        }
        Some(Self {
            size: side,
            rgba: out.into_raw(),
        })
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "icon sizes are at most a few hundred"
)]
fn f32_of(side: u32) -> f32 {
    side as f32
}

fn paint(rgb: [u8; 3]) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(rgb[0], rgb[1], rgb[2], 255));
    paint.anti_alias = true;
    paint
}

fn disc(pixmap: &mut Pixmap, cx: f32, cy: f32, radius: f32, rgb: [u8; 3]) {
    if let Some(path) = PathBuilder::from_circle(cx, cy, radius) {
        pixmap.fill_path(
            &path,
            &paint(rgb),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

/// The strokes of each digit in a box 4 wide and 6 high; each inner list is one pen stroke.
fn glyph(digit: usize) -> Vec<Vec<(f32, f32)>> {
    let six = vec![
        (3.6, 0.3),
        (2.5, 0.0),
        (1.0, 0.4),
        (0.0, 2.0),
        (0.0, 5.0),
        (1.0, 6.0),
        (3.0, 6.0),
        (4.0, 5.0),
        (4.0, 3.8),
        (3.0, 2.8),
        (1.0, 2.8),
        (0.0, 3.8),
    ];
    match digit {
        0 => vec![vec![
            (1.0, 0.0),
            (3.0, 0.0),
            (4.0, 1.0),
            (4.0, 5.0),
            (3.0, 6.0),
            (1.0, 6.0),
            (0.0, 5.0),
            (0.0, 1.0),
            (1.0, 0.0),
        ]],
        1 => vec![vec![(1.0, 1.5), (2.2, 0.0), (2.2, 6.0)]],
        2 => vec![vec![
            (0.0, 1.2),
            (1.0, 0.0),
            (3.0, 0.0),
            (4.0, 1.0),
            (4.0, 2.5),
            (0.0, 6.0),
            (4.0, 6.0),
        ]],
        3 => vec![
            vec![
                (0.0, 0.5),
                (0.8, 0.0),
                (3.2, 0.0),
                (4.0, 1.0),
                (4.0, 2.0),
                (3.0, 3.0),
                (1.5, 3.0),
            ],
            vec![
                (3.0, 3.0),
                (4.0, 4.0),
                (4.0, 5.0),
                (3.2, 6.0),
                (0.8, 6.0),
                (0.0, 5.5),
            ],
        ],
        4 => vec![vec![(3.0, 6.0), (3.0, 0.0), (0.0, 4.2), (4.0, 4.2)]],
        5 => vec![vec![
            (4.0, 0.0),
            (0.4, 0.0),
            (0.2, 2.8),
            (2.8, 2.6),
            (4.0, 3.6),
            (4.0, 5.0),
            (3.0, 6.0),
            (1.0, 6.0),
            (0.0, 5.0),
        ]],
        6 => vec![six],
        7 => vec![vec![(0.0, 0.0), (4.0, 0.0), (1.6, 6.0)]],
        8 => vec![vec![
            (1.0, 3.0),
            (0.2, 2.0),
            (0.2, 1.0),
            (1.0, 0.0),
            (3.0, 0.0),
            (3.8, 1.0),
            (3.8, 2.0),
            (3.0, 3.0),
            (1.0, 3.0),
            (0.0, 4.0),
            (0.0, 5.0),
            (1.0, 6.0),
            (3.0, 6.0),
            (4.0, 5.0),
            (4.0, 4.0),
            (3.0, 3.0),
        ]],
        _ => vec![six.iter().map(|&(x, y)| (4.0 - x, 6.0 - y)).collect()],
    }
}

fn strokes(pixmap: &mut Pixmap, paths: &[Vec<(f32, f32)>], at: Transform, width: f32) {
    let stroke = Stroke {
        width,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    for points in paths {
        let mut builder = PathBuilder::new();
        for (i, &(x, y)) in points.iter().enumerate() {
            if i == 0 {
                builder.move_to(x, y);
            } else {
                builder.line_to(x, y);
            }
        }
        if let Some(path) = builder.finish() {
            pixmap.stroke_path(&path, &paint(WHITE), &stroke, at, None);
        }
    }
}

/// A white digit centred on `(cx, cy)`, `height` tall.
fn digit(pixmap: &mut Pixmap, digit: usize, cx: f32, cy: f32, height: f32) {
    let unit = height / 6.0;
    let at = Transform::from_translate(cx - 2.0 * unit, cy - 3.0 * unit).pre_scale(unit, unit);
    // the pen is in glyph units, so its width is the stroke wanted in pixels over the unit
    let width = (height * 0.22).max(1.4) / unit;
    strokes(pixmap, &glyph(digit), at, width);
}

/// A white plus centred on `(cx, cy)`, `span` wide; `size` is the overlay's side, for the pen.
fn plus(pixmap: &mut Pixmap, cx: f32, cy: f32, span: f32, size: f32) {
    let half = span / 2.0;
    let at = Transform::identity();
    let cross = vec![
        vec![(cx - half, cy), (cx + half, cy)],
        vec![(cx, cy - half), (cx, cy + half)],
    ];
    strokes(pixmap, &cross, at, (size * 0.1).max(1.2));
}

/// A pixmap with `base`'s pixels (which are straight alpha; tiny-skia keeps them premultiplied).
fn pixmap_of(base: &Icon) -> Option<Pixmap> {
    let expected = (base.size as usize)
        .checked_mul(base.size as usize)?
        .checked_mul(4)?;
    if base.size == 0 || base.rgba.len() != expected {
        return None;
    }
    let mut pixmap = Pixmap::new(base.size, base.size)?;
    for (pixel, &[red, green, blue, alpha]) in pixmap
        .pixels_mut()
        .iter_mut()
        .zip(base.rgba.as_chunks::<4>().0)
    {
        let premultiply =
            |c: u8| u8::try_from((u32::from(c) * u32::from(alpha) + 127) / 255).unwrap_or(255);
        *pixel = PremultipliedColorU8::from_rgba(
            premultiply(red),
            premultiply(green),
            premultiply(blue),
            alpha,
        )?;
    }
    Some(pixmap)
}

fn icon_of(pixmap: &Pixmap) -> Icon {
    let mut rgba = Vec::with_capacity(pixmap.data().len());
    for pixel in pixmap.pixels() {
        let c = pixel.demultiply();
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Icon {
        size: pixmap.width(),
        rgba,
    }
}

#[cfg(test)]
mod tests;
