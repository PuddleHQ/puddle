// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared pieces of the corporate-lab tests: origins and clients everywhere, the sandbox rig
//! where the guest agent exists (Unix).

#[cfg(target_os = "linux")]
mod link_shim;
#[cfg(unix)]
mod ntlm;
mod origin;
#[cfg(unix)]
mod sandbox;

#[cfg(target_os = "linux")]
pub(crate) use link_shim::*;
#[cfg(unix)]
pub(crate) use ntlm::*;
pub(crate) use origin::*;
#[cfg(unix)]
pub(crate) use sandbox::*;
mod git;
pub(crate) use git::*;
