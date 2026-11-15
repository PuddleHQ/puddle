// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_api::MemorySettings;

use super::*;

fn prefs(choice: Option<AppIconChoice>) -> UiPrefs {
    let mut prefs = UiPrefs::default();
    prefs.app_icon = choice;
    prefs
}

fn own_png() -> Vec<u8> {
    Icon {
        size: SIZE,
        rgba: [10, 20, 30, 255].repeat((SIZE * SIZE) as usize),
    }
    .to_png()
    .unwrap()
}

#[test]
fn the_default_is_the_duck_and_a_built_in_reads_no_file() {
    let repo = MemorySettings::default();
    repo.save_app_icon(&own_png()).unwrap();
    let wanted = Wanted::read(&repo, &prefs(None));
    assert_eq!(wanted.choice, AppIconChoice::Duck);
    assert_eq!(wanted.custom, None, "the image is only read when chosen");
    assert_eq!(
        wanted.draw().unwrap(),
        built_in(AppIconChoice::Duck, SIZE).unwrap()
    );
    let night = Wanted::read(&repo, &prefs(Some(AppIconChoice::DuckNight)));
    assert_eq!(
        night.draw().unwrap(),
        built_in(AppIconChoice::DuckNight, SIZE).unwrap()
    );
    assert_ne!(night, wanted);
}

#[test]
fn your_own_image_is_the_icon_and_a_replaced_image_is_a_different_wish() {
    let repo = MemorySettings::default();
    repo.save_app_icon(&own_png()).unwrap();
    let custom = prefs(Some(AppIconChoice::Custom));
    let first = Wanted::read(&repo, &custom);
    let icon = first.draw().unwrap();
    assert_eq!(icon.size, SIZE);
    assert_eq!(icon.rgba[..4], [10, 20, 30, 255]);
    assert_eq!(
        first,
        Wanted::read(&repo, &custom),
        "unchanged: nothing to redraw"
    );

    let other = Icon {
        size: SIZE,
        rgba: [1, 2, 3, 255].repeat((SIZE * SIZE) as usize),
    };
    repo.save_app_icon(&other.to_png().unwrap()).unwrap();
    let second = Wanted::read(&repo, &custom);
    assert_ne!(first, second);
    assert_eq!(second.draw().unwrap(), other);
}

#[test]
fn a_missing_or_damaged_own_image_falls_back_to_the_duck() {
    let duck = built_in(AppIconChoice::Duck, SIZE).unwrap();
    let repo = MemorySettings::default();
    let custom = prefs(Some(AppIconChoice::Custom));
    assert_eq!(Wanted::read(&repo, &custom).draw().unwrap(), duck);
    repo.save_app_icon(b"not a png").unwrap();
    let damaged = Wanted::read(&repo, &custom);
    assert_eq!(damaged.draw().unwrap(), duck);
}

/// A store whose image cannot be read.
struct Broken;

impl SettingsRepo for Broken {
    fn load_global(&self) -> Result<Option<serde_json::Value>, puddle_api::SettingsRepoError> {
        Ok(None)
    }
    fn save_global(&self, _: serde_json::Value) -> Result<(), puddle_api::SettingsRepoError> {
        Ok(())
    }
    fn load_workspace(
        &self,
        _: &puddle_types::WorkspaceId,
    ) -> Result<Option<serde_json::Value>, puddle_api::SettingsRepoError> {
        Ok(None)
    }
    fn save_workspace(
        &self,
        _: &puddle_types::WorkspaceId,
        _: serde_json::Value,
    ) -> Result<(), puddle_api::SettingsRepoError> {
        Ok(())
    }
    fn delete_workspace(
        &self,
        _: &puddle_types::WorkspaceId,
    ) -> Result<(), puddle_api::SettingsRepoError> {
        Ok(())
    }
    fn load_app_icon(&self) -> Result<Option<Vec<u8>>, puddle_api::SettingsRepoError> {
        Err(puddle_api::SettingsRepoError::new("permission denied"))
    }
    fn save_app_icon(&self, _: &[u8]) -> Result<(), puddle_api::SettingsRepoError> {
        Ok(())
    }
    fn delete_app_icon(&self) -> Result<(), puddle_api::SettingsRepoError> {
        Ok(())
    }
}

#[test]
fn an_image_that_cannot_be_read_shows_the_duck() {
    // The rest of the stand-in answers as an empty store.
    let name = puddle_types::WorkspaceId::new("box").unwrap();
    assert_eq!(Broken.load_global().unwrap(), None);
    Broken.save_global(serde_json::json!({})).unwrap();
    assert_eq!(Broken.load_workspace(&name).unwrap(), None);
    Broken.save_workspace(&name, serde_json::json!({})).unwrap();
    Broken.save_app_icon(b"x").unwrap();
    Broken.delete_app_icon().unwrap();
    Broken.delete_workspace(&name).unwrap();
    let wanted = Wanted::read(&Broken, &prefs(Some(AppIconChoice::Custom)));
    assert_eq!(
        wanted.draw().unwrap(),
        built_in(AppIconChoice::Duck, SIZE).unwrap()
    );
}

#[test]
fn the_image_for_tauri_keeps_the_pixels() {
    let icon = built_in(AppIconChoice::DuckSun, 32).unwrap();
    let shown = image(&icon);
    assert_eq!((shown.width(), shown.height()), (32, 32));
    assert_eq!(shown.rgba(), icon.rgba);
}
