// SPDX-License-Identifier: AGPL-3.0-or-later
//! `puddle-lab`: the company-network lab (Squid, a TLS-intercepting proxy, an NTLM proxy and a PAC
//! server) as a standing program. See `puddle_e2e::lab::standing` for the flags.
#![forbid(unsafe_code)]

fn main() -> std::process::ExitCode {
    puddle_e2e::lab::standing::main(std::env::args().skip(1))
}
