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
struct Layer(&'static str, Option<Box<Self>>);

impl std::fmt::Display for Layer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Layer {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.1
            .as_deref()
            .map(|l| -> &(dyn std::error::Error + 'static) { l })
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

fn resolved(layers: usize, total: Option<u64>) -> MsbPull {
    MsbPull::Resolved {
        reference: "img".into(),
        manifest_digest: "sha256:m".into(),
        layer_count: layers,
        total_download_bytes: total,
    }
}

fn downloading(layer_index: usize, downloaded_bytes: u64, total_bytes: Option<u64>) -> MsbPull {
    MsbPull::LayerDownloadProgress {
        layer_index,
        digest: "sha256:l".into(),
        downloaded_bytes,
        total_bytes,
    }
}

fn unpacking(layer_index: usize, bytes_read: u64, total_bytes: u64) -> MsbPull {
    MsbPull::LayerMaterializeProgress {
        layer_index,
        bytes_read,
        total_bytes,
    }
}

#[test]
fn layers_downloading_side_by_side_add_up_against_the_manifest_total() {
    let mut tally = Tally::default();
    let first = tally.apply(&resolved(2, Some(1000))).unwrap();
    assert_eq!(
        first,
        PullProgress {
            phase: PullPhase::Downloading,
            done: 0,
            total: Some(1000)
        }
    );
    tally.apply(&downloading(0, 100, Some(600)));
    let now = tally.apply(&downloading(1, 250, Some(400))).unwrap();
    assert_eq!((now.done, now.total), (350, Some(1000)));
    let later = tally.apply(&downloading(0, 600, Some(600))).unwrap();
    assert_eq!(later.done, 850, "a layer's count replaces its earlier one");
}

#[test]
fn a_manifest_without_sizes_gets_its_total_from_the_layers_once_all_said_theirs() {
    let mut tally = Tally::default();
    tally.apply(&resolved(2, None));
    let one = tally.apply(&downloading(0, 10, Some(100))).unwrap();
    assert_eq!(one.total, None, "one layer's size is not the image's");
    let both = tally.apply(&downloading(1, 10, Some(300))).unwrap();
    assert_eq!((both.done, both.total), (20, Some(400)));
}

#[test]
fn unpacking_counts_from_zero_and_names_a_total_only_when_every_layer_has_one() {
    let mut tally = Tally::default();
    tally.apply(&resolved(2, Some(10)));
    let first = tally.apply(&unpacking(0, 50, 200)).unwrap();
    assert_eq!(first.phase, PullPhase::Unpacking);
    assert_eq!((first.done, first.total), (50, None));
    let second = tally.apply(&unpacking(1, 0, 100)).unwrap();
    assert_eq!((second.done, second.total), (50, Some(300)));
}

#[test]
fn events_that_move_no_count_report_nothing() {
    let mut tally = Tally::default();
    assert_eq!(tally.apply(&MsbPull::StitchComplete), None);
    assert_eq!(
        tally.apply(&MsbPull::Resolving {
            reference: "img".into()
        }),
        None
    );
}

#[test]
fn a_finished_layer_counts_as_fully_downloaded_and_gives_its_size() {
    let mut tally = Tally::default();
    tally.apply(&resolved(1, None));
    let done = tally
        .apply(&MsbPull::LayerDownloadComplete {
            layer_index: 0,
            digest: "sha256:l".into(),
            downloaded_bytes: 300,
        })
        .unwrap();
    assert_eq!((done.done, done.total), (300, Some(300)));
}

#[tokio::test]
async fn dropping_the_guard_ends_a_task_that_has_not_finished() {
    let task = tokio::spawn(std::future::pending::<()>());
    let probe = task.abort_handle();
    drop(EndsOnDrop(task));
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    assert!(probe.is_finished());
}

/// A real connect failure from the client the registry client is built on: nothing listens on
/// the port.
async fn refused_connect() -> reqwest::Error {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    reqwest::get(format!("http://127.0.0.1:{port}/v2/"))
        .await
        .unwrap_err()
}

#[tokio::test]
async fn a_failed_connect_says_which_registry_could_not_be_reached() {
    let err = ImageError::Registry(OciDistributionError::RequestError(refused_connect().await));
    let text = explain_connect(&err, "registry.test");
    assert!(text.starts_with("could not connect to registry.test: "), "{text}");
}

#[test]
fn a_reason_that_is_not_a_failed_connect_is_left_alone() {
    let err = ImageError::Registry(OciDistributionError::UnauthorizedError {
        url: "https://registry.test/v2/".to_owned(),
    });
    assert_eq!(explain_connect(&err, "registry.test"), chain(&err));
}

#[test]
fn a_connect_look_alike_in_the_text_is_not_a_failed_connect() {
    let err = ImageError::ManifestParse("client error (Connect)".to_owned());
    assert_eq!(explain_connect(&err, "registry.test"), chain(&err));
}
