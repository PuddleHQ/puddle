// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests of the capture of one workspace's logins: what a token endpoint's answer becomes, what
//! is kept, what survives a restart and what is never done.

use std::sync::{Arc, Mutex};

use puddle_proxy::{AnswerHead, ExchangeRewriter, Rewrite, StandIns, Want};
use puddle_secrets::{MemoryStore, SecretStore};
use puddle_types::{ReasonCode, WorkspaceId};
use serde_json::{Value, json};

use crate::profile::{CLAUDE, DOCKER, GITHUB, OPENAI, Role, builtin};
use crate::test_rig::{Rig, answered_bytes, assert_refused, host, rig_with};
use crate::vault::Vault;
use crate::{ForgetError, Logins, Marker, Problem};

const REAL_ACCESS: &str =
    "sk-ant-oat01-CANARY0access0real0AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const REAL_REFRESH: &str =
    "sk-ant-ort01-CANARY0refresh0real0BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
const REAL_KEY: &str = "sk-ant-api03-CANARY0key0real0CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC-end-of-key-0123456789";
const REAL_GH: &str = "gho_CANARY0github0real0DDDDDDDDDDDDD";
const ACCESS_B: &str =
    "sk-ant-oat01-CANARY0access0second0EEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE";
const REFRESH_B: &str =
    "sk-ant-ort01-CANARY0refresh0second0FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF";

fn rig_on(store: &Arc<MemoryStore>, enabled: bool) -> Rig {
    rig_marked(store, enabled, Marker::none())
}

fn rig_marked(store: &Arc<MemoryStore>, enabled: bool, marker: Marker) -> Rig {
    rig_with(builtin(), store, enabled, marker)
}

fn rig() -> Rig {
    rig_on(&Arc::new(MemoryStore::new()), true)
}

fn claude_answer(access: &str, refresh: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "token_type": "Bearer",
        "access_token": access,
        "expires_in": 28800,
        "refresh_token": refresh,
        "scope": "user:inference user:profile",
        "account": {"uuid": "u-1", "email_address": "me@example.invalid"},
    }))
    .unwrap()
}

/// The JSON the guest gets in place of the answer, or `None` after checking it is the refusal.
#[track_caller]
fn given_or_refused(rewrite: Rewrite) -> Option<Value> {
    match rewrite {
        Rewrite::Replace(bytes) => Some(serde_json::from_slice(&bytes).unwrap()),
        other => {
            assert_refused(&other);
            None
        }
    }
}

async fn claude_login(rig: &Rig, access: &str, refresh: &str) -> Value {
    claude_login_at(rig, "platform.claude.com", access, refresh).await
}

async fn claude_login_at(rig: &Rig, token_host: &str, access: &str, refresh: &str) -> Value {
    let exchange = rig
        .logins
        .begin(&host(token_host), "POST", "/v1/oauth/token");
    let body = answered_bytes(
        exchange,
        200,
        Some("application/json"),
        &claude_answer(access, refresh),
    )
    .await
    .expect("the answer was captured");
    serde_json::from_slice(&body).unwrap()
}

mod capture;
mod kept;
mod read_again;
mod sessions;
mod store_timeouts;
