# Vendored tokio-yamux (temporary)

A copy of tokio-yamux 0.3.20 with two fixes, wired in through `[patch.crates-io]` in the workspace
`Cargo.toml`. `NOTICE` says what differs from the release and the conditions for deleting this
directory. The licence is the upstream MIT licence (`LICENSE`).

**Why:** tokio-yamux 0.3.20 stops handling a stream's frames once the peer's FIN has arrived, so the
peer's window updates are lost and nothing wakes a writer blocked on a full window. A client that
sends its request, shuts down its write side and then downloads more than one stream window (16 MiB
here) stalls at the window: `curl` does not half-close, but `nc -N`, Python's `shutdown(SHUT_WR)` and
Node's `socket.end()` do. The same stall hits an upload when the server half-closes early.

The same release also wakes a writer blocked on a full window only through the stream's reader: the
window update reaches the stream through a channel that only `poll_read` listened on. A program that
only writes to a stream (the proxy forwarding a plain-HTTP response, which reads nothing more from the
guest after the request) stalled for good once the window was spent, at 16 MiB to 24 MiB of the
download. The channel's waker now wakes the reader and the writer both, so a writer gets its window
update whether or not anyone reads. Regression tests: `crates/puddle-agent-proto/tests/yamux_window.rs`
`a_stream_that_is_only_written_sends_past_its_window`, `crates/puddle-proxy/tests/plain_http_window.rs`
and the transparency suite's `a_large_plain_http_response_comes_down_with_no_stall`.

**Second fix, why:** the session stopped reading its socket as soon as one stream's reader fell behind
(frame channel of 8 full), so a guest client that reads slowly or not at all (a paused download, a tool
that extracts while it downloads) stalled every other connection on the same session, for as long as it
stayed behind. One in four connections on the agent's default of 4 sessions. The session now keeps
reading, bounds the data each stream holds to its window (a peer that ignores it gets that stream
reset, and the stream's reader gets a connection-reset error, never a clean end) and what all of them
hold to four windows. Past four windows held by streams that do not read, the session stops reading as
before: five paused downloads of a full window each on one session still stall it. The frames
waiting for a slow stream are held per stream, and their weight is counted as they come and go: 0.3.20
went over every waiting frame for each frame received, so a reader that fell a window behind (2 048
frames of 8 KiB) made each new frame cost thousands of steps, and a gibibyte down to a one-vCPU guest
took 30 s against 5 s up. Regression tests:

- `crates/puddle-agent/tests/relay.rs`: `a_client_that_stops_reading_does_not_stall_the_other_connections_of_its_session`
- `crates/puddle-agent-proto/tests/yamux_window.rs`: `a_stream_sent_past_its_window_while_unread_is_reset_and_the_session_goes_on`,
  `a_stream_holding_a_full_window_and_a_close_while_unread_still_reads_to_the_end` and
  `a_window_of_small_frames_held_for_a_late_reader_moves_in_linear_time`
- `crates/puddle-e2e/tests/transparency.rs`: `a_client_reading_one_kibibyte_a_second_for_a_minute_is_not_cut_off`

**What is not here:** upstream's tests, benches and examples. The regression tests live in the
product's own crates and move 40 MiB through a half-closed connection:

- `crates/puddle-proxy/tests/tunnel_half_close.rs` (the proxy's spliced tunnel)
- `crates/puddle-agent/tests/relay.rs`: `a_client_that_half_closes_still_gets_a_download_larger_than_the_stream_window`
  and `a_server_that_half_closes_still_gets_an_upload_larger_than_the_stream_window` (the guest agent)

**Removal:** see `NOTICE`. When it is time, delete this directory and the patch entry, run every
test named above, then `cargo update -p tokio-yamux` to take the new release.
