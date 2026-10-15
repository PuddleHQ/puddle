// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared pieces of the corporate-lab tests: origins and clients everywhere, the sandbox rig
//! where the guest agent exists (Unix).
#![allow(unused_imports, reason = "each test binary uses some of the pieces")]

mod ntlm;
mod origin;
#[cfg(unix)]
mod sandbox;

pub(crate) use ntlm::*;
pub(crate) use origin::*;
#[cfg(unix)]
pub(crate) use sandbox::*;
