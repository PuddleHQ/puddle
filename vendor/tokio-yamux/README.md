# Vendored tokio-yamux (temporary)

A copy of tokio-yamux 0.3.20 with one fix, wired in through `[patch.crates-io]` in the workspace
`Cargo.toml`. `NOTICE` says what differs from the release and the conditions for deleting this
directory. The licence is the upstream MIT licence (`LICENSE`).

**Why:** tokio-yamux 0.3.20 stops handling a stream's frames once the peer's FIN has arrived, so the
peer's window updates are lost and nothing wakes a writer blocked on a full window. A client that
sends its request, shuts down its write side and then downloads more than one stream window (16 MiB
here) stalls at the window: `curl` does not half-close, but `nc -N`, Python's `shutdown(SHUT_WR)` and
Node's `socket.end()` do. The same stall hits an upload when the server half-closes early.

**What is not here:** upstream's tests, benches and examples. The regression tests live in the
product's own crates and move 40 MiB through a half-closed connection:

- `crates/puddle-proxy/tests/tunnel_half_close.rs` (the proxy's spliced tunnel)
- `crates/puddle-agent/tests/relay.rs`: `a_client_that_half_closes_still_gets_a_download_larger_than_the_stream_window`
  and `a_server_that_half_closes_still_gets_an_upload_larger_than_the_stream_window` (the guest agent)

**Removal:** see `NOTICE`. When it is time, delete this directory and the patch entry, run the three
tests above, then `cargo update -p tokio-yamux` to take the new release.
