// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;

use super::*;

#[test]
fn oci_config_becomes_puddles_image_config() {
    let oci = microsandbox_image::ImageConfig {
        env: vec![
            "PATH=/usr/bin:/bin".into(),
            "EMPTY=".into(),
            "A=b=c".into(),
            "BARE".into(),
        ],
        cmd: Some(vec!["bash".into()]),
        entrypoint: Some(vec!["dockerd-entrypoint.sh".into()]),
        working_dir: Some("/src".into()),
        user: Some("vscode".into()),
        labels: HashMap::from([
            ("devcontainer.metadata".to_owned(), "[]".to_owned()),
            ("a".to_owned(), "1".to_owned()),
        ]),
        ..Default::default()
    };
    let c = convert(oci);
    assert_eq!(c.entrypoint, ["dockerd-entrypoint.sh"]);
    assert_eq!(c.cmd, ["bash"]);
    assert_eq!(c.env_var("PATH"), Some("/usr/bin:/bin"));
    assert_eq!(c.env_var("EMPTY"), Some(""));
    assert_eq!(c.env_var("A"), Some("b=c"));
    assert_eq!(c.env_var("BARE"), Some(""));
    assert_eq!(c.working_dir.as_deref(), Some("/src"));
    assert_eq!(c.user.as_deref(), Some("vscode"));
    assert_eq!(
        c.labels.keys().collect::<Vec<_>>(),
        ["a", "devcontainer.metadata"]
    );
}

#[derive(Debug)]
struct Layer(&'static str, Option<Box<Layer>>);

impl std::fmt::Display for Layer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Layer {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.1.as_deref().map(|l| l as _)
    }
}

#[test]
fn an_error_shows_its_causes_once_each() {
    let err = Layer(
        "error sending request",
        Some(Box::new(Layer(
            "client error (Connect)",
            Some(Box::new(Layer(
                "invalid peer certificate: UnknownIssuer",
                None,
            ))),
        ))),
    );
    assert_eq!(
        chain(&err),
        "error sending request: client error (Connect): invalid peer certificate: UnknownIssuer"
    );
    let repeats = Layer("registry error: boom", Some(Box::new(Layer("boom", None))));
    assert_eq!(chain(&repeats), "registry error: boom");
}

#[test]
fn puddles_roots_come_after_msbs_own() {
    let roots = ["-----BEGIN CERTIFICATE-----\nA\n".to_owned()];
    assert_eq!(
        with_roots(vec![b"msb".to_vec()], &roots),
        [b"msb".to_vec(), roots[0].as_bytes().to_vec()]
    );
    assert_eq!(with_roots(Vec::new(), &[]), Vec::<Vec<u8>>::new());
}

#[test]
fn missing_and_empty_fields_are_none_or_empty() {
    let c = convert(microsandbox_image::ImageConfig {
        working_dir: Some(String::new()),
        user: Some(String::new()),
        ..Default::default()
    });
    assert!(c.entrypoint.is_empty() && c.cmd.is_empty() && c.env.is_empty());
    assert_eq!(c.working_dir, None);
    assert_eq!(c.user, None);
    assert!(c.labels.is_empty());
}
