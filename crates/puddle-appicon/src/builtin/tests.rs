// SPDX-License-Identifier: AGPL-3.0-or-later
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
fn the_rubber_duck_is_a_side_view_with_a_glossy_highlight_and_no_water() {
    let icon = built_in(AppIconChoice::RubberDuck, 64).unwrap();
    assert_eq!(at(&icon, 1, 1)[3], 0, "the corner is clear");
    assert_eq!(at(&icon, 52, 62)[3], 0, "no water under it");
    assert_eq!(at(&icon, 28, 50), [0xf6, 0xc4, 0x31, 0xff], "the body");
    assert_eq!(at(&icon, 55, 25)[..3], BEAK, "the beak");
    assert_ne!(at(&icon, 34, 12)[..3], DUCK, "the highlight on the head");
}

#[test]
fn the_front_view_has_two_eyes_a_beak_in_the_middle_and_feet() {
    let icon = built_in(AppIconChoice::DuckFront, 64).unwrap();
    assert_eq!(at(&icon, 1, 1)[3], 0, "the corner is clear");
    assert_eq!(at(&icon, 25, 19)[..3], INK, "the left eye");
    assert_eq!(at(&icon, 38, 19)[..3], INK, "the right eye");
    assert_eq!(at(&icon, 32, 28)[..3], BEAK, "the beak");
    assert_eq!(at(&icon, 32, 45)[..3], DUCK, "the belly");
    assert_eq!(at(&icon, 30, 61)[..3], BEAK, "the feet");
}

#[test]
fn the_built_in_designs_differ_from_each_other() {
    let designs = [
        AppIconChoice::Duck,
        AppIconChoice::RubberDuck,
        AppIconChoice::DuckFront,
    ]
    .map(|c| built_in(c, 64).unwrap());
    assert_ne!(designs[0], designs[1]);
    assert_ne!(designs[0], designs[2]);
    assert_ne!(designs[1], designs[2]);
}

#[test]
fn every_built_in_draws_at_the_sizes_the_shell_and_settings_use() {
    for choice in [
        AppIconChoice::Duck,
        AppIconChoice::RubberDuck,
        AppIconChoice::DuckFront,
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
    let icon = built_in(AppIconChoice::DuckFront, 32).unwrap();
    assert_eq!(Icon::from_png(&icon.to_png().unwrap()).unwrap(), icon);
}
