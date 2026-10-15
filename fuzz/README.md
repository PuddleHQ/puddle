# Fuzz targets

cargo-fuzz targets for what a guest's first bytes reach before any rule is asked (HG-10 in
`docs/arch/threat-model.md`). They need a nightly toolchain, so this crate is outside the workspace.

| Target | What it feeds |
|---|---|
| `head_parser` | a proxied stream's first bytes: request head, target, normalised host |
| `connect_target` | the target of a `CONNECT` or absolute-form request, as text |
| `normalise_host` | `puddle_netpolicy::normalise_host` |
| `agent_preamble` | the first line of an agent stream |

The checks live in `puddle_proxy::fuzzing` (behind the `fuzzing` feature), so the stable test run
replays the same code: `crates/puddle-proxy/tests/fuzz_corpus.rs` runs every file under
`corpus/<target>/`.

```sh
cargo install cargo-fuzz --locked
rustup toolchain install nightly --profile minimal
cd fuzz && cargo +nightly fuzz run head_parser -- -max_total_time=600
```

`.github/workflows/fuzz.yml` runs each target for ten minutes every night. When a run crashes,
copy the input from `fuzz/artifacts/<target>/` into `corpus/<target>/` under a name that says
what it was (`crash-<what>`), fix the code, and the replay test keeps it fixed.
