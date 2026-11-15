// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_settings::{Consent, ConsentKind, MICROSOFT_TERMS_VERSION, TermsVersion, UnixMillis};

use super::*;

fn microsoft(consent: bool) -> GlobalSettings {
    let mut global = GlobalSettings::default();
    global.vscode_server.server = Some(ServerChoice::Microsoft);
    if consent {
        global.consents.set(
            ConsentKind::VsCodeServer,
            Consent::Granted {
                at: UnixMillis(1),
                terms_version: TermsVersion::new(MICROSOFT_TERMS_VERSION).unwrap(),
            },
        );
    }
    global
}

#[test]
fn r41_code_server_by_default_gives_open_vsx_only() {
    let plan = plan(&GlobalSettings::default(), &[], &[]);
    assert_eq!(
        plan.everywhere.into_iter().collect::<Vec<_>>(),
        [SystemReason::CodeServer]
    );
    assert_eq!(plan.workspaces.len(), 0);
}

#[test]
fn r41_microsoft_server_needs_the_consent() {
    let with = plan(&microsoft(true), &[], &[]);
    assert_eq!(
        with.everywhere.into_iter().collect::<Vec<_>>(),
        [SystemReason::MicrosoftServer]
    );
    // Chosen without a consent: the editor runs code-server, so that is what is allowed.
    assert_eq!(
        plan(&microsoft(false), &[], &[]),
        plan(&GlobalSettings::default(), &[], &[])
    );
}

#[test]
fn r41_a_consent_to_other_terms_does_not_open_microsofts_hosts() {
    let mut global = microsoft(true);
    global.consents.set(
        ConsentKind::VsCodeServer,
        Consent::Granted {
            at: UnixMillis(1),
            terms_version: TermsVersion::new("https://example.test/older-terms").unwrap(),
        },
    );
    let plan = plan(&global, &[], &[]);
    assert!(!plan.everywhere.contains(&SystemReason::MicrosoftServer));
    assert!(plan.everywhere.contains(&SystemReason::CodeServer));
}

#[test]
fn r41_direct_ssh_gives_microsofts_hosts_to_that_workspace_only() {
    let ssh = WorkspaceId::new("ssh").unwrap();
    let plan = plan(&GlobalSettings::default(), std::slice::from_ref(&ssh), &[]);
    assert_eq!(
        plan.workspaces
            .get(&ssh)
            .map(|r| r.iter().copied().collect::<Vec<_>>()),
        Some(DIRECT_SSH_REASONS.to_vec())
    );
    assert!(!plan.everywhere.contains(&SystemReason::DirectSsh));
}

#[test]
fn m8_the_mise_tools_set_is_hidden_exactly_while_the_global_switch_is_off() {
    assert!(
        plan(&GlobalSettings::default(), &[], &[])
            .hidden_sets
            .is_empty()
    );
    let mut off = GlobalSettings::default();
    off.tools.mise = Some(false);
    assert_eq!(
        plan(&off, &[], &[])
            .hidden_sets
            .into_iter()
            .collect::<Vec<_>>(),
        [MISE_TOOLS_SLUG]
    );
    off.tools.mise = Some(true);
    assert!(plan(&off, &[], &[]).hidden_sets.is_empty());
}

#[test]
fn mise_hosts_go_to_the_workspaces_with_mise_on_only() {
    let on = WorkspaceId::new("tools").unwrap();
    let plan = plan(&GlobalSettings::default(), &[], std::slice::from_ref(&on));
    assert_eq!(
        plan.workspaces
            .get(&on)
            .map(|r| r.iter().copied().collect::<Vec<_>>()),
        Some(vec![SystemReason::Mise])
    );
    assert!(!plan.everywhere.contains(&SystemReason::Mise));
    assert_eq!(plan.workspaces.len(), 1);
}

#[test]
fn the_switches_decide_who_has_mise_on() {
    let mut global = GlobalSettings::default();
    let mut own = WorkspaceSettings::default();
    assert!(mise_on(&global, &own), "on by default");
    own.tools.mise = Some(false);
    assert!(!mise_on(&global, &own), "the workspace's own switch");
    own.tools.mise = Some(true);
    global.tools.mise = Some(false);
    assert!(!mise_on(&global, &own), "the global switch wins");
}
