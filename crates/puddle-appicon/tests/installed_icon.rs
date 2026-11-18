// SPDX-License-Identifier: AGPL-3.0-or-later
//! The icon files the installer and the executable are built from are the duck.
//!
//! `build.rs` (through Tauri) embeds `icons/icon.ico` as the executable's icon resource, and the
//! installer template takes the same file for its shortcut and its entry in the installed-apps
//! list. A plain yellow disc in that file shipped once, so the Start menu and Explorer showed a
//! disc while the tray, drawn by `puddle-appicon`, showed the duck. These tests render the duck
//! the way the tray does and compare it with the committed files (the test lives in this crate,
//! which draws the duck, because the shell crate does not link on a host without GTK).
//!
//! After the duck's drawing changes, `PUDDLE_BLESS_ICONS=1 cargo test -p puddle-appicon --test
//! installed_icon` rewrites the files.

#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "helpers outside a #[test] fn: a failed read or a malformed file fails the test"
)]

use std::path::{Path, PathBuf};

use puddle_appicon::{Icon, built_in};
use puddle_settings::AppIconChoice;

/// The sizes inside `icon.ico`: the ones Windows picks from for the title bar (16), the taskbar and
/// Start menu (24, 32, 48, 64 at its scale steps), Explorer's large view (256) and the shortcut.
const ICO_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

/// The PNG files Tauri's `bundle.icon` lists, with their side.
const PNG_FILES: [(&str, u32); 3] = [("32x32.png", 32), ("128x128.png", 128), ("icon.png", 256)];

fn icons_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../puddle-app/icons")
}

fn duck(size: u32) -> Icon {
    built_in(AppIconChoice::Duck, size).expect("the duck is a built-in choice")
}

/// An ICO file whose images are all PNG: a six-byte header, a 16-byte entry per image, the images.
fn ico_of(sizes: &[u32]) -> Vec<u8> {
    let pngs: Vec<Vec<u8>> = sizes
        .iter()
        .map(|&s| duck(s).to_png().expect("the duck encodes"))
        .collect();
    let mut out = vec![0, 0, 1, 0];
    out.extend(u16::try_from(sizes.len()).unwrap().to_le_bytes());
    let mut offset = 6 + 16 * sizes.len();
    for (&size, png) in sizes.iter().zip(&pngs) {
        // A side of 256 is stored as 0.
        let side = u8::try_from(size).unwrap_or(0);
        out.extend([side, side, 0, 0, 1, 0, 32, 0]);
        out.extend(u32::try_from(png.len()).unwrap().to_le_bytes());
        out.extend(u32::try_from(offset).unwrap().to_le_bytes());
        offset += png.len();
    }
    for png in pngs {
        out.extend(png);
    }
    out
}

/// The PNG images of an ICO file, by side.
fn ico_images(ico: &[u8]) -> Vec<(u32, Icon)> {
    assert_eq!(&ico[..4], [0, 0, 1, 0], "an ICO header");
    let count = usize::from(u16::from_le_bytes([ico[4], ico[5]]));
    (0..count)
        .map(|i| {
            let entry = &ico[6 + 16 * i..22 + 16 * i];
            let side = if entry[0] == 0 {
                256
            } else {
                u32::from(entry[0])
            };
            let len = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
            let at = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
            let icon = Icon::from_png(&ico[at..at + len]).expect("a PNG image in the ICO");
            (side, icon)
        })
        .collect()
}

fn bless() -> bool {
    std::env::var_os("PUDDLE_BLESS_ICONS").is_some()
}

#[test]
fn ico_holds_the_duck_at_every_size() {
    let path = icons_dir().join("icon.ico");
    if bless() {
        std::fs::write(&path, ico_of(&ICO_SIZES)).unwrap();
    }
    let images = ico_images(&std::fs::read(&path).unwrap());
    let sides: Vec<u32> = images.iter().map(|(side, _)| *side).collect();
    assert_eq!(sides, ICO_SIZES, "the sizes in icons/icon.ico");
    for (side, icon) in images {
        assert!(icon == duck(side), "the {side} px image is not the duck");
    }
}

#[test]
fn png_files_are_the_duck() {
    for (name, side) in PNG_FILES {
        let path = icons_dir().join(name);
        if bless() {
            std::fs::write(&path, duck(side).to_png().unwrap()).unwrap();
        }
        let icon = Icon::from_png(&std::fs::read(&path).unwrap()).unwrap();
        assert!(icon == duck(side), "icons/{name} is not the duck");
    }
}
