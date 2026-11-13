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
