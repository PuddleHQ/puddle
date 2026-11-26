// SPDX-License-Identifier: AGPL-3.0-or-later
//! Software known to keep workspaces from working as expected. One entry per program: add a program
//! here and the system check finds it, with no other code to write.

/// One program the system check looks for.
#[derive(Debug, PartialEq, Eq)]
pub struct ProblematicSoftware {
    /// The name shown to the person.
    pub name: &'static str,
    /// How it is detected: the Windows services it installs. It counts as installed when any exists.
    pub services: &'static [&'static str],
    /// Why it matters for workspaces.
    pub why: &'static str,
    /// What to do about it.
    pub remedy: &'static str,
}

/// Every program the check looks for.
pub const PROBLEMATIC_SOFTWARE: &[ProblematicSoftware] = &[ProblematicSoftware {
    name: "Microsoft Entra Global Secure Access client",
    services: &[
        "GlobalSecureAccessEngineService",
        "GlobalSecureAccessTunnelingService",
        "GlobalSecureAccessPolicyRetrieverService",
        "GlobalSecureAccessClientManagerService",
    ],
    why: "Microsoft documents that this client doesn't tunnel virtual machines' traffic and isn't \
supported on devices that host VMs. Workspaces have no network of their own: their traffic \
leaves through Puddle's proxy, an ordinary program on this computer, which the client handles \
like any other app.",
    remedy: "If company resources reached through Global Secure Access don't work from a workspace, \
this limitation is the likely cause. Puddle can't change it: ask your IT team for another way to \
reach them.",
}];

#[cfg(test)]
mod tests;
