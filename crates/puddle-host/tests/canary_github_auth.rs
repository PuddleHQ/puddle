// SPDX-License-Identifier: AGPL-3.0-or-later
//! The test extension that stands in for VS Code's `github` sign-in in the desktop VS Code VM test
//! (`fixtures/canary-github-auth`), run on node against a stand-in `vscode` module: it registers the
//! `github` provider only when a canary token is given, answers every request with that token and
//! nothing else, and logs each request.

use std::ffi::OsStr;
use std::time::Duration;

use puddle_vm_tests::Node;
use serde_json::Value;

/// Loads the extension with a `vscode` module that keeps the provider it is given, activates it,
/// asks the provider the way the git extension's askpass does and prints what it saw.
const HARNESS: &str = r"
const Module = require('node:module');
const real = Module._load;
let registered = null;
const fake = {
  EventEmitter: class { constructor() { this.event = () => {}; } dispose() {} },
  authentication: {
    registerAuthenticationProvider: (id, label, provider, options) => {
      registered = { id, label, provider, options };
      return { dispose() {} };
    },
  },
};
Module._load = (request, ...rest) => (request === 'vscode' ? fake : real(request, ...rest));
const extension = require(process.argv[1]);
const context = { subscriptions: [] };
extension.activate(context);
(async () => {
  const out = { registered: registered !== null, subscriptions: context.subscriptions.length };
  if (registered) {
    out.id = registered.id;
    out.multiple = registered.options.supportsMultipleAccounts;
    out.got = await registered.provider.getSessions(['repo', 'workflow']);
    out.gotNone = await registered.provider.getSessions(undefined);
    out.created = await registered.provider.createSession(['repo']);
    await registered.provider.removeSession('puddle-canary');
  }
  console.log(JSON.stringify(out));
})();
";

fn extension() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("canary-github-auth")
        .join("extension.js")
}

#[expect(
    clippy::expect_used,
    reason = "a test helper: a failed step fails the test"
)]
async fn activate(envs: &[(&str, &OsStr)]) -> Value {
    let out = Node::from_env()
        .run(
            [
                OsStr::new("-e"),
                OsStr::new(HARNESS),
                OsStr::new("--"),
                extension().as_os_str(),
            ],
            envs,
            "",
            Duration::from_secs(30),
        )
        .await
        .expect("node runs");
    assert!(out.success, "{}", out.stderr);
    serde_json::from_str(out.stdout.trim()).expect("one JSON line")
}

#[tokio::test]
async fn the_canary_is_the_only_token_the_github_provider_hands_out_and_every_request_is_logged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("asked.log");
    let seen = activate(&[
        ("PUDDLE_CANARY_TOKEN", OsStr::new("gho_canary")),
        ("PUDDLE_CANARY_LOG", log.as_os_str()),
    ])
    .await;
    assert_eq!(seen["registered"], true, "{seen}");
    assert_eq!(seen["id"], "github");
    assert_eq!(seen["multiple"], false);
    // The provider and its event emitter are both disposed with the extension.
    assert_eq!(seen["subscriptions"], 2);
    for sessions in [&seen["got"], &seen["gotNone"]] {
        assert_eq!(sessions.as_array().unwrap().len(), 1, "{seen}");
        assert_eq!(sessions[0]["accessToken"], "gho_canary", "{seen}");
        assert_eq!(sessions[0]["account"]["id"], "puddle-canary");
    }
    assert_eq!(
        seen["got"][0]["scopes"],
        serde_json::json!(["repo", "workflow"])
    );
    assert_eq!(seen["gotNone"][0]["scopes"], serde_json::json!([]));
    assert_eq!(seen["created"]["accessToken"], "gho_canary");
    assert_eq!(
        std::fs::read_to_string(&log)
            .expect("the log")
            .lines()
            .collect::<Vec<_>>(),
        [
            "registered",
            "getSessions repo,workflow",
            "getSessions ",
            "createSession repo"
        ]
    );
}

#[tokio::test]
async fn without_a_canary_the_extension_registers_nothing() {
    let seen = activate(&[]).await;
    assert_eq!(seen["registered"], false, "{seen}");
    assert_eq!(seen["subscriptions"], 0);
}
