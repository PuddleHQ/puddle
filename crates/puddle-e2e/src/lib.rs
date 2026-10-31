// SPDX-License-Identifier: GPL-3.0-or-later
//! Test harness for end-to-end and hostile-guest tests (tiers L2-L4 and the hostile-guest tiers P/V/W): fake upstreams, guest drivers, audit-log assertions; the corporate-network fixtures ([`lab`]) and the UI fixture backend ([`ui_fixture`]). Test and development code only; never a dependency of product crates.
// Windows: the lab reads its fixtures' process trees through Toolhelp and GetProcessTimes.
#![cfg_attr(not(windows), forbid(unsafe_code))]

pub mod lab;
pub mod ui_fixture;
