// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared pieces of the corporate-lab tests: origins and clients everywhere, the sandbox rig
//! where the guest agent exists (Unix).

#[cfg(unix)]
mod ntlm;
mod origin;
#[cfg(unix)]
mod sandbox;

#[cfg(unix)]
pub(crate) use ntlm::*;
pub(crate) use origin::*;
#[cfg(unix)]
pub(crate) use sandbox::*;
mod git;
pub(crate) use git::*;
