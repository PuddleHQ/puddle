// SPDX-License-Identifier: AGPL-3.0-or-later
//! The node half of a client, as a process: JSON lines in, one JSON line back per command.
//!
//! Shared by the browser client (`browser.rs`) and the desktop VS Code client (`vscode.rs`); each
//! has its own driver script and its own commands, and this is only the transport: spawn, call
//! with a budget, give up on a driver that hangs or ends, and read the answer.

use std::ffi::OsStr;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};

use crate::browser::{Node, PageOutcome};
use crate::error::HarnessError;

/// A running driver script. Dropping it kills the node process.
#[derive(Debug)]
pub(crate) struct Driver {
    grace: Duration,
    /// Names a step that got no answer in time, in [`HarnessError::Timeout`].
    label: &'static str,
    /// Names the closing step, in [`HarnessError::Timeout`].
    closing: &'static str,
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: Lines<BufReader<tokio::process::ChildStdout>>,
}

impl Driver {
    /// Starts `node` on `script` (an ES module's source) with `envs` added.
    ///
    /// `grace` is how long node gets beyond a step's own budget.
    pub(crate) fn spawn(
        node: &Node,
        script: &str,
        envs: &[(&str, &OsStr)],
        grace: Duration,
        label: &'static str,
        closing: &'static str,
    ) -> Result<Self, HarnessError> {
        let mut command = node.command(["--input-type=module", "-e", script]);
        command.envs(envs.iter().copied());
        let mut child = command.spawn().map_err(|source| {
            HarnessError::io("start node", PathBuf::from(&node.program), source)
        })?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().map(|out| BufReader::new(out).lines());
        let (Some(stdin), Some(stdout)) = (stdin, stdout) else {
            return Err(HarnessError::Browser {
                detail: "node gave no pipes".to_owned(),
            });
        };
        Ok(Self {
            grace,
            label,
            closing,
            child,
            stdin,
            stdout,
        })
    }

    /// Asks the driver to quit and waits for node to end.
    pub(crate) async fn quit(mut self) -> Result<(), HarnessError> {
        let _ = self.stdin.write_all(b"{\"op\":\"quit\"}\n").await;
        let _ = self.stdin.flush().await;
        match tokio::time::timeout(self.grace, self.child.wait()).await {
            Ok(_) => Ok(()),
            Err(_) => Err(HarnessError::Timeout {
                what: self.closing,
                limit: self.grace,
            }),
        }
    }

    /// Sends one command and reads its answer, waiting at most `budget` plus the grace.
    pub(crate) async fn call(
        &mut self,
        command: &Value,
        budget: Duration,
    ) -> Result<PageOutcome, HarnessError> {
        let mut line = command.to_string();
        line.push('\n');
        // A driver that has ended refuses the write or gives no answer; either way its stderr says why.
        let sent = match self.stdin.write_all(line.as_bytes()).await {
            Ok(()) => self.stdin.flush().await,
            Err(e) => Err(e),
        };
        if sent.is_err() {
            return Err(self.ended().await);
        }
        let limit = budget + self.grace;
        let answer = tokio::time::timeout(limit, self.stdout.next_line())
            .await
            .map_err(|_| HarnessError::Timeout {
                what: self.label,
                limit,
            })?;
        match answer {
            Ok(Some(answer)) => parse_reply(&answer),
            Ok(None) | Err(_) => Err(self.ended().await),
        }
    }

    /// The error for a driver that ended: what it wrote to stderr (read once).
    async fn ended(&mut self) -> HarnessError {
        let mut stderr = String::new();
        if let Some(mut err) = self.child.stderr.take() {
            let _ =
                tokio::time::timeout(Duration::from_secs(5), err.read_to_string(&mut stderr)).await;
        }
        HarnessError::Browser {
            detail: format!(
                "the driver ended without an answer; stderr: {}",
                stderr.trim()
            ),
        }
    }
}

/// Reads one of the driver's answers.
fn parse_reply(line: &str) -> Result<PageOutcome, HarnessError> {
    let fail = |detail: String| HarnessError::Browser { detail };
    let reply: Value =
        serde_json::from_str(line).map_err(|e| fail(format!("unreadable driver answer: {e}")))?;
    let field = |key: &str| reply.get(key).unwrap_or(&Value::Null);
    let strings = |key: &str| -> Vec<String> {
        field(key)
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect()
    };
    let console = strings("console");
    let page_errors = strings("pageErrors");
    if field("ok").as_bool() != Some(true) {
        return Err(fail(format!(
            "{}; console: {console:?}; page errors: {page_errors:?}",
            field("error").as_str().unwrap_or("no reason given")
        )));
    }
    Ok(PageOutcome {
        result: field("result").clone(),
        console,
        page_errors,
    })
}

#[cfg(test)]
mod tests;
