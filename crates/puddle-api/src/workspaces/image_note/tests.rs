// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn a_dev_container_image_is_put_into_words() {
    let got = describe_image(&labels(&[
        ("dev.containers.id", "base-debian"),
        ("dev.containers.variant", "trixie"),
        ("dev.containers.release", "v0.4.39"),
        (
            "devcontainer.metadata",
            r#"[{"id":"ghcr.io/devcontainers/features/common-utils:2"},{"id":"ghcr.io/devcontainers/features/git:1","customizations":{}},{"remoteUser":"vscode"}]"#,
        ),
    ]));
    assert_eq!(
        got.as_deref(),
        Some("Debian trixie dev container image v0.4.39 with common-utils, git")
    );
}

#[test]
fn the_images_own_description_wins_and_keeps_only_its_first_line() {
    let got = describe_image(&labels(&[
        (
            "org.opencontainers.image.description",
            "A small box.\nSecond line",
        ),
        ("dev.containers.id", "base-debian"),
    ]));
    assert_eq!(got.as_deref(), Some("A small box."));
}

#[test]
fn labels_that_say_nothing_give_no_line() {
    assert_eq!(describe_image(&labels(&[("version", "2.2.7")])), None);
    assert_eq!(describe_image(&BTreeMap::new()), None);
}

#[test]
fn broken_metadata_is_left_out_and_a_long_label_is_cut() {
    let got = describe_image(&labels(&[
        ("dev.containers.id", "base-ubuntu"),
        ("devcontainer.metadata", "not json"),
    ]));
    assert_eq!(got.as_deref(), Some("Ubuntu dev container"));
    let long = "x".repeat(500);
    let cut = describe_image(&labels(&[("org.opencontainers.image.description", &long)])).unwrap();
    assert_eq!(cut.chars().count(), MAX_CHARS);
    assert!(cut.ends_with('\u{2026}'));
}
