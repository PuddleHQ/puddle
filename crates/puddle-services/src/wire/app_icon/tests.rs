// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn every_choice_converts_both_ways_unchanged() {
    for choice in [
        settings::AppIconChoice::Duck,
        settings::AppIconChoice::DuckPond,
        settings::AppIconChoice::DuckNight,
        settings::AppIconChoice::DuckSun,
        settings::AppIconChoice::Custom,
    ] {
        assert_eq!(
            settings::AppIconChoice::from(AppIconChoice::from(choice)),
            choice
        );
    }
    assert_eq!(
        serde_json::to_string(&AppIconChoice::DuckSun).unwrap(),
        r#""duck_sun""#
    );
}
