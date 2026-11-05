# Fuzz targets

cargo-fuzz targets for what a guest's first bytes reach before any rule is asked (in
`docs/arch/threat-model.md`). They need a nightly toolchain, so this crate is outside the workspace.

| Target | What it feeds |
|---|---|
| `head_parser` | a proxied stream's first bytes: request head, target, normalised host |
| `connect_target` | the target of a `CONNECT` or absolute-form request, as text |
| `normalise_host` | `puddle_netpolicy::normalise_host` |
| `agent_preamble` | the first line of an agent stream |
| `resolve_line` | the lookup lines the host reads from the agent (`puddle_agent_proto::fuzzing`) |
| `dns_message` | the DNS message the guest's stub reads from a tool in the guest (`checks/dns_message.rs`) |
| `resolve_line_grammar` | the same lines, built from a grammar (JSON spellings, key order, escapes, unknown fields, broken variants) that knows what the host must make of each (`puddle_agent_proto::fuzzing::resolve_line_grammar`) |
| `dns_message_grammar` | the same messages, built from the wire grammar (header counts, labels, compression pointers that loop or point forward, records, `OPT`, cut or padded tails) that knows what the stub must make of each (`checks/dns_message_grammar.rs`) |

The grammar targets decode the fuzzer's bytes into a structure with `arbitrary`, so most inputs
reach past the header and the first label, where a byte-level input rarely gets. They run the
byte-level checks on the bytes they build too. The byte-level targets stay: they reach the shapes
no grammar writes.

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
