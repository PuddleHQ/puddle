// SPDX-License-Identifier: AGPL-3.0-or-later
//! The workspace-service contract suite, run on `FakeWorkspaces` over real HTTP. The host's own
//! tests run the same suite on the real service, so the two cannot drift apart.

use futures_util::future::LocalBoxFuture;
use puddle_api::contract::{self, Reply, Subject};
use serde_json::Value;

use crate::common::{Api, start};

struct Over<'a>(&'a Api);

impl Subject for Over<'_> {
    fn call<'a>(
        &'a self,
        method: &'static str,
        path: &'a str,
        body: Option<Value>,
    ) -> LocalBoxFuture<'a, Reply> {
        Box::pin(async move {
            let reply = self.0.send(method, path, body.as_ref()).await;
            Reply {
                status: reply.status,
                body: serde_json::from_str(&reply.body).unwrap_or(Value::Null),
            }
        })
    }
}

#[tokio::test]
async fn the_in_memory_workspace_service_keeps_the_contract() {
    let api = start().await;
    contract::run(&Over(&api)).await;
}
