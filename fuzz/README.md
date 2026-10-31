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

The checks live in `puddle_proxy::fuzzing` and `puddle_agent_proto::fuzzing` (behind each crate's `fuzzing` feature) and in `checks/` here (the shipped agent has no such feature), so the stable test run
replays the same code: `fuzz_corpus.rs` in the tests of `puddle-proxy`, `puddle-agent-proto` and
`puddle-agent` runs every file under `corpus/<target>/`.

```sh
cargo install cargo-fuzz --locked
rustup toolchain install nightly --profile minimal
cd fuzz && cargo +nightly fuzz run head_parser -- -max_total_time=600
```

`.github/workflows/fuzz.yml` runs each target for ten minutes every night. When a run crashes,
copy the input from `fuzz/artifacts/<target>/` into `corpus/<target>/` under a name that says
what it was (`crash-<what>`), fix the code, and the replay test keeps it fixed.
