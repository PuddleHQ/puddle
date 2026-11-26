// SPDX-License-Identifier: AGPL-3.0-or-later
//! The workspace-service contract suite, run on the real service over a fake runtime. The API's
//! own tests run the same suite on the in-memory service.

use futures_util::future::LocalBoxFuture;
use puddle_services::contract::{self, Reply, Subject};
use serde_json::Value;

use super::{Rig, api};
use crate::support::Api;

struct Over<'a>(&'a Api);

impl Subject for Over<'_> {
    fn call<'a>(
        &'a self,
        method: &'static str,
        path: &'a str,
        body: Option<Value>,
    ) -> LocalBoxFuture<'a, Reply> {
        Box::pin(async move {
            let text = body.map(|b| b.to_string());
            let reply = self.0.request(method, path, text.as_deref(), true).await;
            Reply {
                status: reply.status,
                body: serde_json::from_str(&reply.body).unwrap_or(Value::Null),
            }
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_real_workspace_service_keeps_the_contract() {
    let rig = Rig::new();
    let host = rig.start().await;
    contract::run(&Over(&api(&host))).await;
}
