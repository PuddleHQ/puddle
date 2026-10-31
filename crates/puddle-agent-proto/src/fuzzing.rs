// SPDX-License-Identifier: GPL-3.0-or-later
//! Entry points for the `resolve_line` cargo-fuzz target: the lookup lines the host reads from
//! the guest agent (and the agent reads back). Each function takes arbitrary bytes, never panics,
//! and panics only when an invariant the lines rest on does not hold.

use futures_util::FutureExt;

use crate::resolve::{MAX_QUERY_LINE, ResolveAnswer, ResolveQuery, read_query};

/// Reads `bytes` as a query line, as a stream of query lines and as an answer line.
///
/// # Panics
///
/// When a query the parser accepted does not write and read back as itself, when the stream
/// reader takes more than [`MAX_QUERY_LINE`] bytes for one query, or when an answer the parser
/// accepted does not write and read back as itself.
pub fn resolve_line(bytes: &[u8]) {
    if let Ok(query) = ResolveQuery::from_line(bytes)
        && let Ok(line) = query.to_line()
    {
        let again = ResolveQuery::from_line(&line);
        assert_eq!(
            again.ok().as_ref(),
            Some(&query),
            "an accepted query does not read back as itself"
        );
    }

    let mut reader = bytes;
    // A slice never waits, so the read finishes on its first poll.
    if let Some(Ok(streamed)) = read_query(&mut reader).now_or_never() {
        let taken = bytes.get(..bytes.len() - reader.len()).unwrap_or_default();
        assert!(
            taken.len() <= MAX_QUERY_LINE,
            "a query line over the limit was accepted"
        );
        assert_eq!(
            ResolveQuery::from_line(taken).ok(),
            Some(streamed),
            "the stream reader and the line parser disagree"
        );
    }

    if let Ok(answer) = ResolveAnswer::from_line(bytes)
        && let Ok(line) = answer.to_line()
    {
        let again = ResolveAnswer::from_line(&line);
        assert_eq!(
            again.ok().as_ref(),
            Some(&answer),
            "an accepted answer does not read back as itself"
        );
    }
}
