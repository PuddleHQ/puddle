// SPDX-License-Identifier: AGPL-3.0-or-later
//! One line that says what an image is, read from the image's own labels.

use std::collections::BTreeMap;

/// The most characters of a description the New workspace form shows; the labels are the image
/// author's, so a long one is cut.
const MAX_CHARS: usize = 160;

/// What the image says it is, in one line, or `None` when its labels say nothing usable.
///
/// The image's own `org.opencontainers.image.description` wins. Without it, a dev container
/// image's labels are put into words: `dev.containers.id` (`base-debian`), `.variant`
/// (`trixie`), `.release`, and the feature names in `devcontainer.metadata`. Every value is
/// untrusted text from the registry, so only the first line is kept, cut to a fixed length.
#[must_use]
pub fn describe_image(labels: &BTreeMap<String, String>) -> Option<String> {
    let label = |k: &str| labels.get(k).map(|v| v.trim()).filter(|v| !v.is_empty());
    if let Some(own) = label("org.opencontainers.image.description") {
        return Some(clip(own));
    }
    let id = label("dev.containers.id")?;
    let os = id.strip_prefix("base-").unwrap_or(id);
    let mut line = capitalised(os);
    if let Some(variant) = label("dev.containers.variant") {
        line.push(' ');
        line.push_str(variant);
    }
    line.push_str(" dev container");
    if let Some(release) = label("dev.containers.release") {
        line.push_str(" image ");
        line.push_str(release);
    }
    let features = features(label("devcontainer.metadata"));
    if !features.is_empty() {
        line.push_str(" with ");
        line.push_str(&features.join(", "));
    }
    Some(clip(&line))
}

/// The names of the features the metadata label lists (`ghcr.io/devcontainers/features/git:1`
/// is `git`), in order, each once.
fn features(metadata: Option<&str>) -> Vec<String> {
    let Some(Ok(serde_json::Value::Array(entries))) =
        metadata.map(serde_json::from_str::<serde_json::Value>)
    else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    for id in entries.iter().filter_map(|e| e.get("id")?.as_str()) {
        let name = id.rsplit('/').next().unwrap_or(id);
        let name = name.split([':', '@']).next().unwrap_or(name);
        if !name.is_empty() && !names.iter().any(|n| n == name) {
            names.push(name.to_owned());
        }
    }
    names
}

fn capitalised(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

fn clip(text: &str) -> String {
    let first = text.lines().next().unwrap_or("").trim();
    if first.chars().count() <= MAX_CHARS {
        return first.to_owned();
    }
    let mut cut: String = first.chars().take(MAX_CHARS - 1).collect();
    cut.push('\u{2026}');
    cut
}

#[cfg(test)]
mod tests;
