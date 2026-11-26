// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

const SIDES: [u32; 4] = [16, 20, 24, 32];

fn at(icon: &Icon, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * icon.size + x) * 4) as usize;
    icon.rgba[i..i + 4].try_into().unwrap()
}

/// Distance of the middle of pixel (`x`, `y`) from the middle of the icon, in icon radii.
fn radii(icon: &Icon, x: u32, y: u32) -> f32 {
    let half = f32_of(icon.size) / 2.0;
    let (dx, dy) = (f32_of(x) + 0.5 - half, f32_of(y) + 0.5 - half);
    (dx * dx + dy * dy).sqrt() / half
}

#[test]
fn the_overlay_side_follows_the_display_scale() {
    let sides: Vec<u32> = [1.0, 1.25, 1.5, 2.0].map(overlay_side).to_vec();
    assert_eq!(sides, SIDES);
    assert_eq!(overlay_side(0.5), 16, "never below the base size");
    assert_eq!(overlay_side(f64::NAN), 16);
}

#[test]
fn the_dot_edge_fades_out_in_steps_and_is_never_white() {
    for side in SIDES {
        for count in [0, 3, 12] {
            let icon = overlay(count, side).unwrap();
            let mut alphas = std::collections::BTreeSet::new();
            for y in 0..side {
                for x in 0..side {
                    let [r, g, b, a] = at(&icon, x, y);
                    if radii(&icon, x, y) > 0.92 && a > 0 {
                        alphas.insert(a);
                        // a faint pixel loses precision when its alpha is taken back out
                        let near = |c: u8, want: u8| c.abs_diff(want) <= 24;
                        assert!(
                            near(r, DOT[0]) && near(g, DOT[1]) && near(b, DOT[2]),
                            "{side}px count {count} at {x},{y}: {r},{g},{b}"
                        );
                    }
                }
            }
            assert!(
                alphas.len() >= 4,
                "{side}px count {count}: a smooth edge has several alpha steps, got {alphas:?}"
            );
        }
    }
}

#[test]
fn the_overlay_is_a_transparent_square_with_a_red_disc() {
    for side in SIDES {
        let icon = overlay(0, side).unwrap();
        assert_eq!(
            (icon.size, icon.rgba.len()),
            (side, (side * side * 4) as usize)
        );
        assert_eq!(at(&icon, 0, 0), [0, 0, 0, 0], "corner at {side}px");
        assert_eq!(at(&icon, side - 1, side - 1), [0, 0, 0, 0]);
        assert_eq!(at(&icon, side / 2, side / 2), [DOT[0], DOT[1], DOT[2], 255]);
    }
}

/// How many pixels of the overlay are nearly white and opaque (the digit's strokes).
fn white_pixels(icon: &Icon) -> usize {
    icon.rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] == 255 && p[0] > 230 && p[1] > 230 && p[2] > 230)
        .count()
}

#[test]
fn each_digit_is_white_on_the_dot_and_inside_it() {
    for side in SIDES {
        let mut seen = Vec::new();
        for count in 1..=9 {
            let icon = overlay(count, side).unwrap();
            assert!(
                white_pixels(&icon) >= 4,
                "digit {count} at {side}px is visible"
            );
            for y in 0..side {
                for x in 0..side {
                    if at(&icon, x, y)[..3] == WHITE && at(&icon, x, y)[3] > 0 {
                        assert!(
                            radii(&icon, x, y) < 0.9,
                            "digit {count} stays inside the dot"
                        );
                    }
                }
            }
            assert!(
                !seen.contains(&icon.rgba),
                "digit {count} differs from the others"
            );
            seen.push(icon.rgba);
        }
    }
}

#[test]
fn more_than_nine_shows_a_nine_and_a_plus() {
    for side in SIDES {
        assert_ne!(overlay(9, side), overlay(10, side));
        assert_eq!(overlay(10, side), overlay(500, side));
        assert!(white_pixels(&overlay(10, side).unwrap()) >= 6);
    }
    assert!(overlay(1, 0).is_none());
}

fn grey(side: u32) -> Icon {
    Icon {
        size: side,
        rgba: [0x20, 0x40, 0x60, 0xff].repeat((side * side) as usize),
    }
}

#[test]
fn the_tray_dot_is_red_in_the_lower_right_and_leaves_the_rest_alone() {
    let base = grey(32);
    let marked = with_dot(&base, false).unwrap();
    assert_eq!(at(&marked, 23, 23), [DOT[0], DOT[1], DOT[2], 255]);
    assert_eq!(at(&marked, 2, 2), at(&base, 2, 2));
    assert_eq!(at(&marked, 2, 29), at(&base, 2, 29));
    assert_eq!(at(&marked, 29, 2), at(&base, 29, 2));
    let alphas: std::collections::BTreeSet<u8> = marked
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[..3] != [0x20, 0x40, 0x60])
        .map(|p| p[3])
        .collect();
    assert!(
        alphas.len() == 1,
        "over an opaque icon the edge blends into it: {alphas:?}"
    );
}

#[test]
fn the_tray_ring_is_white_and_only_there_when_asked_for() {
    let base = grey(32);
    let plain = with_dot(&base, false).unwrap();
    let ringed = with_dot(&base, true).unwrap();
    let white = |i: &Icon| {
        i.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[..3] == WHITE)
            .count()
    };
    assert_eq!(white(&plain), 0, "no white without the ring");
    assert!(white(&ringed) >= 10, "the ring shows");
    assert_eq!(at(&ringed, 23, 23)[..3], DOT, "the dot is still on top");
}

#[test]
fn a_dot_cannot_be_drawn_on_a_malformed_icon() {
    let torn = Icon {
        size: 4,
        rgba: vec![0; 10],
    };
    assert!(with_dot(&torn, false).is_none());
    assert!(
        with_dot(
            &Icon {
                size: 0,
                rgba: vec![]
            },
            false
        )
        .is_none()
    );
}

#[test]
fn a_soft_edge_resizes_without_a_dark_fringe() {
    let mut icon = Icon {
        size: 8,
        rgba: vec![0; 8 * 8 * 4],
    };
    for y in 2_usize..6 {
        for x in 2_usize..6 {
            let at = (y * 8 + x) * 4;
            icon.rgba[at..at + 4].copy_from_slice(&[0xf6, 0xc4, 0x31, 0xff]);
        }
    }
    let small = icon.resized(4).unwrap();
    assert_eq!(small.size, 4);
    for p in small.rgba.as_chunks::<4>().0.iter().filter(|p| p[3] > 8) {
        assert!(p[0] >= 0xe8 && p[1] >= 0xb0, "fringe pixel {p:?}");
    }
    assert!(icon.resized(0).is_none());
}
