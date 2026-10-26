// SPDX-License-Identifier: GPL-3.0-or-later
//! The app icon through the API: the icons to choose between, the upload of the user's own image,
//! and the rule that the icon in use always exists.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use puddle_api::SettingsRepo;
use puddle_appicon::{Icon, built_in};
use puddle_settings::AppIconChoice;
use serde_json::json;

use crate::common::start;

/// A PNG of noise: too big for an ordinary request body, small enough for an upload.
fn noisy_png(side: u32) -> Vec<u8> {
    let mut state = 0x9e37_79b9_u32;
    let rgba = (0..side * side * 4)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state.to_be_bytes()[0]
        })
        .collect();
    Icon { size: side, rgba }.to_png().unwrap()
}

fn decode(b64: &str) -> Icon {
    Icon::from_png(&STANDARD.decode(b64).unwrap()).unwrap()
}

#[tokio::test]
async fn the_built_in_icons_come_drawn_for_a_preview_and_there_is_no_custom_one_yet() {
    let api = start().await;
    let reply = api.get("/api/app-icons").await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let view = reply.json();
    let choices: Vec<_> = view["built_in"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["choice"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(choices, ["duck", "duck_pond", "duck_night", "duck_sun"]);
    for icon in view["built_in"].as_array().unwrap() {
        let drawn = decode(icon["png_base64"].as_str().unwrap());
        assert_eq!(drawn.size, 64);
    }
    // What the window shows is what the preview shows.
    assert_eq!(
        decode(view["built_in"][1]["png_base64"].as_str().unwrap()),
        built_in(AppIconChoice::DuckPond, 64).unwrap()
    );
    assert_eq!(view["custom_png_base64"], json!(null));
    api.running.shutdown().await;
}

#[tokio::test]
async fn an_uploaded_image_is_checked_resized_stored_and_replaced() {
    let api = start().await;
    let wide = Icon {
        size: 100,
        rgba: [10, 200, 30, 255].repeat(100 * 100),
    }
    .to_png()
    .unwrap();
    let reply = api.send_file("PUT", "/api/app-icons/custom", &wide).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let stored = decode(reply.json()["png_base64"].as_str().unwrap());
    assert_eq!(stored.size, 256, "resized");
    assert_eq!(
        STANDARD.encode(api.settings.load_app_icon().unwrap().unwrap()),
        reply.json()["png_base64"].as_str().unwrap(),
        "what the reply shows is what is stored"
    );
    assert_eq!(
        api.get("/api/app-icons").await.json()["custom_png_base64"],
        reply.json()["png_base64"]
    );

    // A body over an ordinary request's limit still goes through: it is a file.
    let big = noisy_png(300);
    assert!(big.len() > 64 * 1024);
    let reply = api.send_file("PUT", "/api/app-icons/custom", &big).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    api.running.shutdown().await;
}

#[tokio::test]
async fn an_image_that_is_not_usable_is_refused_with_the_reason_and_changes_nothing() {
    let api = start().await;
    for (body, why) in [
        (Vec::new(), "empty"),
        (b"not an image at all".to_vec(), "not a PNG, JPEG or WebP"),
        (
            Icon {
                size: 16,
                rgba: vec![255; 16 * 16 * 4],
            }
            .to_png()
            .unwrap(),
            "at least 32",
        ),
    ] {
        let reply = api.send_file("PUT", "/api/app-icons/custom", &body).await;
        assert_eq!(reply.status, 422, "{}", reply.body);
        assert_eq!(reply.error(), "invalid");
        assert!(
            reply.json()["message"].as_str().unwrap().contains(why),
            "{}",
            reply.body
        );
    }
    let reply = api
        .send_file(
            "PUT",
            "/api/app-icons/custom",
            &vec![0; 8 * 1024 * 1024 + 1],
        )
        .await;
    assert_eq!(reply.status, 413, "{}", reply.body);
    assert_eq!(reply.error(), "payload_too_large");
    assert_eq!(api.settings.load_app_icon().unwrap(), None);
    api.running.shutdown().await;
}

#[tokio::test]
async fn your_own_icon_can_only_be_chosen_with_an_image_and_not_removed_while_in_use() {
    let api = start().await;
    let choose = |icon: &str| json!({"ui": {"app_icon": icon}});

    let reply = api
        .send("PUT", "/api/settings", Some(&choose("custom")))
        .await;
    assert_eq!(reply.status, 422, "{}", reply.body);
    assert!(reply.json()["message"].as_str().unwrap().contains("image"));
    assert_eq!(
        api.settings.load_global().unwrap(),
        None,
        "nothing was saved"
    );

    let png = noisy_png(64);
    assert_eq!(
        api.send_file("PUT", "/api/app-icons/custom", &png)
            .await
            .status,
        200
    );
    let reply = api
        .send("PUT", "/api/settings", Some(&choose("custom")))
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.json()["ui"]["app_icon"], "custom");

    let reply = api.send("DELETE", "/api/app-icons/custom", None).await;
    assert_eq!(reply.status, 409, "{}", reply.body);
    assert_eq!(reply.error(), "conflict");
    assert!(
        api.settings.load_app_icon().unwrap().is_some(),
        "still there"
    );

    api.send("PUT", "/api/settings", Some(&choose("duck_sun")))
        .await;
    let reply = api.send("DELETE", "/api/app-icons/custom", None).await;
    assert_eq!(reply.status, 204, "{}", reply.body);
    assert_eq!(api.settings.load_app_icon().unwrap(), None);
    assert_eq!(
        api.get("/api/app-icons").await.json()["custom_png_base64"],
        json!(null)
    );
    assert_eq!(
        api.send("DELETE", "/api/app-icons/custom", None)
            .await
            .status,
        204,
        "again is fine"
    );
    api.running.shutdown().await;
}
