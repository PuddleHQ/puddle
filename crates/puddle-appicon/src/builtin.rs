// SPDX-License-Identifier: AGPL-3.0-or-later
//! The built-in duck icons, drawn from paths so they are sharp at any size.
//!
//! The duck is the one in the app's sidebar (`DuckMark.svelte`), in the same 64 by 64 space and
//! the same colours; `builtin/tests.rs` pins the colours to the pixels.

use puddle_settings::AppIconChoice;
use tiny_skia::{Color, FillRule, Paint, Path, PathBuilder, Pixmap, Transform};

use crate::Icon;

const DUCK: [u8; 3] = [0xf6, 0xc4, 0x31];
const BEAK: [u8; 3] = [0xe8, 0x74, 0x2a];
const INK: [u8; 3] = [0x3a, 0x2c, 0x00];
const WATER: [u8; 3] = [0x8c, 0xc9, 0xdc];
const WHITE: [u8; 3] = [0xff, 0xff, 0xff];

/// The icon for a built-in choice, `size` pixels square. `None` for [`AppIconChoice::Custom`]:
/// that one is the user's own image.
#[must_use]
pub fn built_in(choice: AppIconChoice, size: u32) -> Option<Icon> {
    let mut pixmap = Pixmap::new(size, size)?;
    #[expect(
        clippy::cast_precision_loss,
        reason = "icon sizes are at most a few hundred"
    )]
    let scale = size as f32 / 64.0;
    let on_canvas = Transform::from_scale(scale, scale);
    match choice {
        AppIconChoice::Duck => draw_duck(&mut pixmap, DUCK, on_canvas, true)?,
        AppIconChoice::RubberDuck => draw_rubber_duck(&mut pixmap, on_canvas)?,
        AppIconChoice::DuckFront => draw_duck_front(&mut pixmap, on_canvas)?,
        AppIconChoice::Custom => return None,
    }
    Some(Icon {
        size,
        rgba: demultiply(&pixmap),
    })
}

/// The bath toy: a round head on a plump body, a lifted tail, a glossy highlight.
fn draw_rubber_duck(pixmap: &mut Pixmap, at: Transform) -> Option<()> {
    fill(pixmap, &oval(8.0, 27.0, 44.0, 30.0)?, DUCK, 1.0, at);
    fill(pixmap, &tail_path()?, DUCK, 1.0, at);
    fill(pixmap, &oval(18.0, 36.0, 20.0, 12.0)?, INK, 0.12, at);
    fill(pixmap, &circle(41.0, 21.0, 14.0)?, DUCK, 1.0, at);
    fill(pixmap, &oval(50.0, 21.0, 11.0, 8.0)?, BEAK, 1.0, at);
    fill(pixmap, &circle(46.0, 17.0, 2.4)?, INK, 1.0, at);
    fill(pixmap, &circle(46.8, 16.2, 0.8)?, WHITE, 1.0, at);
    fill(pixmap, &oval(30.0, 10.0, 8.0, 4.0)?, WHITE, 0.55, at);
    fill(pixmap, &oval(13.0, 33.0, 7.0, 3.0)?, WHITE, 0.45, at);
    Some(())
}

/// The duck seen from the front: two eyes, a broad beak, a wing at each side and orange feet.
fn draw_duck_front(pixmap: &mut Pixmap, at: Transform) -> Option<()> {
    fill(pixmap, &oval(24.0, 56.0, 12.0, 6.0)?, BEAK, 1.0, at);
    fill(pixmap, &oval(28.0, 56.0, 12.0, 6.0)?, BEAK, 1.0, at);
    for x in [5.0, 48.0] {
        let wing = oval(x, 34.0, 11.0, 18.0)?;
        fill(pixmap, &wing, DUCK, 1.0, at);
        fill(pixmap, &wing, INK, 0.14, at);
    }
    fill(pixmap, &oval(10.0, 26.0, 44.0, 32.0)?, DUCK, 1.0, at);
    fill(pixmap, &circle(32.0, 23.0, 16.0)?, DUCK, 1.0, at);
    fill(pixmap, &oval(24.0, 24.0, 16.0, 9.0)?, BEAK, 1.0, at);
    for x in [25.5, 38.5] {
        fill(pixmap, &circle(x, 19.0, 2.4)?, INK, 1.0, at);
        fill(pixmap, &circle(x + 0.8, 18.2, 0.8)?, DUCK, 1.0, at);
    }
    Some(())
}

fn draw_duck(pixmap: &mut Pixmap, body: [u8; 3], at: Transform, water: bool) -> Option<()> {
    if water {
        let pond = PathBuilder::from_oval(tiny_skia::Rect::from_xywh(5.0, 50.0, 54.0, 10.0)?)?;
        fill(pixmap, &pond, WATER, 1.0, at);
    }
    fill(pixmap, &body_path()?, body, 1.0, at);
    fill(pixmap, &wing_path()?, INK, 0.12, at);
    fill(pixmap, &circle(40.0, 22.0, 11.0)?, body, 1.0, at);
    fill(pixmap, &beak_path()?, BEAK, 1.0, at);
    fill(pixmap, &circle(43.0, 19.0, 2.0)?, INK, 1.0, at);
    fill(pixmap, &circle(43.6, 18.4, 0.6)?, body, 1.0, at);
    Some(())
}

fn oval(x: f32, y: f32, w: f32, h: f32) -> Option<Path> {
    PathBuilder::from_oval(tiny_skia::Rect::from_xywh(x, y, w, h)?)
}

fn tail_path() -> Option<Path> {
    let mut p = PathBuilder::new();
    p.move_to(12.0, 38.0);
    p.cubic_to(7.0, 34.0, 4.0, 30.0, 3.0, 25.0);
    p.cubic_to(8.0, 26.0, 14.0, 28.0, 18.0, 32.0);
    p.close();
    p.finish()
}

fn circle(cx: f32, cy: f32, r: f32) -> Option<Path> {
    PathBuilder::from_circle(cx, cy, r)
}

/// The body: the favicon's path with its relative curves written out.
fn body_path() -> Option<Path> {
    let mut p = PathBuilder::new();
    p.move_to(9.0, 30.0);
    p.cubic_to(12.0, 31.0, 14.0, 34.0, 18.0, 35.0);
    p.cubic_to(23.0, 36.0, 30.0, 34.0, 36.0, 34.0);
    p.cubic_to(44.0, 34.0, 53.0, 36.0, 54.0, 44.0);
    p.cubic_to(55.0, 51.0, 48.0, 56.0, 34.0, 56.0);
    p.cubic_to(20.0, 56.0, 9.0, 52.0, 9.0, 42.0);
    p.cubic_to(9.0, 38.0, 9.0, 34.0, 9.0, 30.0);
    p.close();
    p.finish()
}

fn wing_path() -> Option<Path> {
    let mut p = PathBuilder::new();
    p.move_to(18.0, 41.0);
    p.cubic_to(23.0, 37.0, 32.0, 37.0, 36.0, 43.0);
    p.cubic_to(31.0, 47.0, 23.0, 47.0, 18.0, 41.0);
    p.close();
    p.finish()
}

fn beak_path() -> Option<Path> {
    let mut p = PathBuilder::new();
    p.move_to(49.5, 21.5);
    p.cubic_to(53.5, 20.9, 57.5, 21.7, 59.5, 23.1);
    p.cubic_to(57.5, 25.5, 53.5, 26.7, 49.5, 26.3);
    p.close();
    p.finish()
}

fn fill(pixmap: &mut Pixmap, path: &Path, rgb: [u8; 3], alpha: f32, at: Transform) {
    let mut paint = Paint::default();
    let mut colour = Color::from_rgba8(rgb[0], rgb[1], rgb[2], 255);
    colour.apply_opacity(alpha);
    paint.set_color(colour);
    paint.anti_alias = true;
    pixmap.fill_path(path, &paint, FillRule::Winding, at, None);
}

/// The pixmap's pixels with the alpha taken back out of the colours (tiny-skia keeps them
/// premultiplied; PNG and the windowing code want them straight).
fn demultiply(pixmap: &Pixmap) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixmap.data().len());
    for pixel in pixmap.pixels() {
        let c = pixel.demultiply();
        out.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    out
}

#[cfg(test)]
mod tests;
