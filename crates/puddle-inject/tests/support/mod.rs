// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the injector tests share: the crate's own `World` and a few checks on a decision.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use puddle_inject::testing::World;
use puddle_proxy::{InjectContext, InjectDecision, Injector as _, RequestView};
use puddle_types::Host;

/// The injector's decision for `method target` on `host` with `headers` (`name: value`) and a body.
pub(crate) trait Ask {
    async fn decide_with(
        &self,
        host: &str,
        method: &str,
        target: &str,
        headers: &[&str],
        body: Option<&[u8]>,
    ) -> InjectDecision;
}

impl Ask for World {
    async fn decide_with(
        &self,
        host: &str,
        method: &str,
        target: &str,
        headers: &[&str],
        body: Option<&[u8]>,
    ) -> InjectDecision {
        let host = Host::parse_normalised(host).unwrap();
        let context = InjectContext {
            workspace: &self.workspace,
            host: &host,
        };
        let mut lines = vec![format!("host: {host}")];
        lines.extend(headers.iter().map(|h| (*h).to_owned()));
        let mut view = RequestView::new(method, target, &lines);
        if let Some(body) = body {
            view = view.with_body(body);
        }
        self.injector.decide(&context, &view).await
    }
}

pub(crate) fn basic(user: &str, token: &str) -> String {
    use base64::Engine as _;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{token}"))
    )
}
