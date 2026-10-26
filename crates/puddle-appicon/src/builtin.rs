// SPDX-License-Identifier: GPL-3.0-or-later
//! The built-in duck icons, drawn from paths so they are sharp at any size.
//!
//! The duck is the one in the app's sidebar (`DuckMark.svelte`), in the same 64 by 64 space and
//! the same colours; a test below pins the colours to the pixels.

use puddle_settings::AppIconChoice;
use tiny_skia::{Color, FillRule, Paint, Path, PathBuilder, Pixmap, Transform};

use crate::Icon;

const DUCK: [u8; 3] = [0xf6, 0xc4, 0x31];
const BEAK: [u8; 3] = [0xe8, 0x74, 0x2a];
const INK: [u8; 3] = [0x3a, 0x2c, 0x00];
const WATER: [u8; 3] = [0x8c, 0xc9, 0xdc];
const POND: [u8; 3] = [0x2b, 0x74, 0x9b];
const NIGHT: [u8; 3] = [0x14, 0x26, 0x3d];
const WHITE: [u8; 3] = [0xff, 0xff, 0xff];

/// The icon for a built-in choice, `size` pixels square. `None` for [`AppIconChoice::Custom`]:
/// that one is the user's own image.
#[must_use]
pub fn built_in(choice: AppIconChoice, size: u32) -> Option<Icon> {
    let (disc, body) = match choice {
        AppIconChoice::Duck => (None, DUCK),
        AppIconChoice::DuckPond => (Some(POND), DUCK),
        AppIconChoice::DuckNight => (Some(NIGHT), DUCK),
        AppIconChoice::DuckSun => (Some(DUCK), WHITE),
        AppIconChoice::Custom => return None,
    };
    let mut pixmap = Pixmap::new(size, size)?;
    #[expect(
        clippy::cast_precision_loss,
        reason = "icon sizes are at most a few hundred"
    )]
    let scale = size as f32 / 64.0;
    let on_canvas = Transform::from_scale(scale, scale);
    if let Some(disc) = disc {
        fill(
            &mut pixmap,
            &circle(32.0, 32.0, 31.5)?,
            disc,
            1.0,
            on_canvas,
        );
        // The duck without its water, a little smaller, centred in the disc.
        let centred = on_canvas
            .pre_translate(32.0, 32.0)
            .pre_scale(0.8, 0.8)
            .pre_translate(-34.5, -35.0);
        draw_duck(&mut pixmap, body, centred, false)?;
    } else {
        draw_duck(&mut pixmap, body, on_canvas, true)?;
    }
    Some(Icon {
        size,
        rgba: demultiply(&pixmap),
    })
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
mod tests {
    use super::*;

    /// The pixel at (`x`, `y`) of an icon drawn 64 pixels square.
    fn at(icon: &Icon, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * icon.size + x) * 4) as usize;
        icon.rgba[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn the_plain_duck_is_yellow_on_a_clear_background_with_water_below() {
        let icon = built_in(AppIconChoice::Duck, 64).unwrap();
        assert_eq!(icon.size, 64);
        assert_eq!(at(&icon, 1, 1)[3], 0, "the corner is clear");
        assert_eq!(at(&icon, 24, 52), [0xf6, 0xc4, 0x31, 0xff], "the body");
        assert_eq!(at(&icon, 52, 24)[..3], BEAK, "the beak");
        assert_eq!(at(&icon, 32, 57), [0x8c, 0xc9, 0xdc, 0xff], "the water");
    }

    #[test]
    fn the_disc_variants_fill_a_disc_and_leave_the_corners_clear() {
        for (choice, disc, body) in [
            (AppIconChoice::DuckPond, POND, DUCK),
            (AppIconChoice::DuckNight, NIGHT, DUCK),
            (AppIconChoice::DuckSun, DUCK, WHITE),
        ] {
            let icon = built_in(choice, 64).unwrap();
            assert_eq!(at(&icon, 0, 0)[3], 0, "{choice:?}: the corner is clear");
            assert_eq!(at(&icon, 32, 2)[..3], disc, "{choice:?}: the disc");
            assert_eq!(at(&icon, 32, 2)[3], 0xff);
            assert_eq!(
                at(&icon, 34, 38)[..3],
                body,
                "{choice:?}: the duck sits on it"
            );
        }
    }

    #[test]
    fn every_built_in_draws_at_the_sizes_the_shell_and_settings_use() {
        for choice in [
            AppIconChoice::Duck,
            AppIconChoice::DuckPond,
            AppIconChoice::DuckNight,
            AppIconChoice::DuckSun,
        ] {
            for size in [16, 64, crate::SIZE] {
                let icon = built_in(choice, size).unwrap();
                assert_eq!(icon.rgba.len(), (size * size * 4) as usize);
                assert!(
                    icon.rgba.chunks(4).any(|p| p[3] == 0xff),
                    "{choice:?} {size}"
                );
            }
        }
        assert!(built_in(AppIconChoice::Custom, 64).is_none());
        assert!(
            built_in(AppIconChoice::Duck, 0).is_none(),
            "no empty canvas"
        );
    }

    #[test]
    fn the_png_of_an_icon_reads_back_the_same() {
        let icon = built_in(AppIconChoice::DuckNight, 32).unwrap();
        assert_eq!(Icon::from_png(&icon.to_png().unwrap()).unwrap(), icon);
    }
}
