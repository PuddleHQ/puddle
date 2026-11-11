# Fuzz targets

cargo-fuzz targets for what a guest's first bytes reach before any rule is asked (in
`docs/arch/threat-model.md`). They need a nightly toolchain, so this crate is outside the workspace.

| Target | What it feeds |
|---|---|
| `head_parser` | a proxied stream's first bytes: request head, target, normalised host |
| `connect_target` | the target of a `CONNECT` or absolute-form request, as text |
| `normalise_host` | `puddle_netpolicy::normalise_host` |
| `agent_preamble` | the first line of an agent stream |
| `answer_tokens` | an answer read for tokens Puddle does not hold, whole and in pieces of a size the first byte sets: both must find the same kinds, each once (`puddle_proxy::fuzzing::answer_tokens`) |
| `resolve_line` | the lookup lines the host reads from the agent (`puddle_agent_proto::fuzzing`) |
| `dns_message` | the DNS message the guest's stub reads from a tool in the guest (`checks/dns_message.rs`) |
| `resolve_line_grammar` | the same lines, built from a grammar (JSON spellings, key order, escapes, unknown fields, broken variants) that knows what the host must make of each (`puddle_agent_proto::fuzzing::resolve_line_grammar`) |
| `dns_message_grammar` | the same messages, built from the wire grammar (header counts, labels, compression pointers that loop or point forward, records, `OPT`, cut or padded tails) that knows what the stub must make of each (`checks/dns_message_grammar.rs`) |
| `resolve_ask` | the agent's side of one lookup (`resolve::ask`) against a scripted host on an in-memory stream: replies at, just under and just over the line limit, cut or empty replies, junk, a host that goes quiet, a reply written in small pieces (`puddle_agent_proto::fuzzing::resolve_ask`) |
| `dns_udp_datagram` | one datagram to the stub's real UDP socket on the loopback address: a grammar-built message, a query whose `OPT` record ends at 512, 1232 or the 4096-byte receive buffer, or raw bytes (`checks/dns_transport.rs`) |
| `dns_tcp_stream` | a TCP byte stream to the stub's connection handler: length-prefixed messages with honest and lying prefixes, the limit and one past it, a stream cut anywhere (`checks/dns_transport.rs`) |

The grammar targets decode the fuzzer's bytes into a structure with `arbitrary`, so most inputs
reach past the header and the first label, where a byte-level input rarely gets. They run the
byte-level checks on the bytes they build too. The byte-level targets stay: they reach the shapes
no grammar writes.

The transport targets compare the transport's answers with what the stub says to the same message called directly, so
what they check is which bytes reach it, in which order and how the answer leaves. UDP uses a socket (the receive buffer
is the length check); TCP uses an in-memory stream, because a server that closes with unread bytes resets the
connection and the answers are lost.

The checks live in `puddle_proxy::fuzzing` and `puddle_agent_proto::fuzzing` (behind each crate's `fuzzing` feature) and in `checks/` here (the shipped agent has no such feature), so the stable test run
replays the same code: `fuzz_corpus.rs` in the tests of `puddle-proxy`, `puddle-agent-proto` and
`puddle-agent` runs every file under `corpus/<target>/`.

```sh
cargo install cargo-fuzz --locked
rustup toolchain install nightly --profile minimal
cd fuzz && cargo +nightly fuzz run head_parser -- -max_total_time=600
```

`.github/workflows/fuzz.yml` runs each target for ten minutes every night and for an hour every Sunday. When a run crashes,
copy the input from `fuzz/artifacts/<target>/` into `corpus/<target>/` under a name that says
what it was (`crash-<what>`), fix the code, and the replay test keeps it fixed.

The corpus of a grammar target is fuzzer input, not a message: `fuzz_corpus` decodes each file the way
the target does. Their files are named `seed-<n>`, and a crash input is kept under the same name rule
as above. To refresh one after the grammar changes, run the target, then
`cargo +nightly fuzz cmin <target>` and rename the result.

Coverage of a target (`cargo +nightly fuzz coverage <target>`, then `llvm-cov show` or `report` on
the result) shows which parser lines its corpus reaches; compare a grammar target with its
byte-level twin before trusting either.
