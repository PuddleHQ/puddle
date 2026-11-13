// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use ServiceState::{Missing, Running, Stopped, Unreadable};

#[test]
fn gsa_state_from_its_services() {
    let installed = |running| GsaFacts::Installed { running };
    assert_eq!(GsaFacts::from_services([]), GsaFacts::NotInstalled);
    assert_eq!(
        GsaFacts::from_services([Missing, Missing]),
        GsaFacts::NotInstalled
    );
    assert_eq!(
        GsaFacts::from_services([Missing, Stopped]),
        installed(Some(false))
    );
    assert_eq!(
        GsaFacts::from_services([Stopped, Running, Unreadable]),
        installed(Some(true))
    );
    // An unreadable service might be the running one: "not running" would be a guess.
    assert_eq!(
        GsaFacts::from_services([Stopped, Unreadable, Missing]),
        installed(None)
    );
    assert_eq!(GsaFacts::from_services([Unreadable]), installed(None));
}

#[test]
fn an_installed_agent_service_is_running_disabled_or_stopped() {
    assert_eq!(AgentService::installed(false, true), AgentService::Running);
    assert_eq!(AgentService::installed(true, true), AgentService::Running);
    assert_eq!(AgentService::installed(true, false), AgentService::Disabled);
    assert_eq!(AgentService::installed(false, false), AgentService::Stopped);
}

#[test]
fn only_a_missing_service_reads_as_not_installed() {
    assert_eq!(ServiceState::from_open_error(1060), Missing);
    // Access denied, a shutting-down service manager, an invalid name: all unproven.
    for code in [5, 1115, 1055, 6, 0] {
        assert_eq!(ServiceState::from_open_error(code), Unreadable, "{code}");
    }
}

#[test]
fn a_failed_job_query_names_the_call_and_the_error() {
    let facts = JobFacts::query_failed("QueryInformationJobObject", 87);
    assert_eq!(
        facts,
        JobFacts::Unreadable {
            detail: "QueryInformationJobObject failed: os error 87".into()
        }
    );
    assert!(!facts.forbids_breakaway());
    assert!(
        JobFacts::Read {
            breakaway_allowed: false
        }
        .forbids_breakaway()
    );
}

#[test]
fn a_failed_code_integrity_query_names_the_status() {
    let facts = CodeIntegrityFacts::query_failed(i32::from_ne_bytes(0xC000_0022_u32.to_ne_bytes()));
    assert!(
        matches!(&facts, CodeIntegrityFacts::Unreadable { detail } if detail.contains("NTSTATUS 0xC0000022")),
        "{facts:?}"
    );
}
