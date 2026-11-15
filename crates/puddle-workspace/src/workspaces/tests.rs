// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn defaults_are_valid() {
    let c = WorkspaceConfig::default();
    assert_eq!(c.default_size, DiskSize::gib(32));
    assert_eq!(
        c.maintenance_image.as_str(),
        WorkspaceConfig::DEFAULT_MAINTENANCE_IMAGE
    );
    assert_eq!(c.maintenance_memory.get(), 1024);
}

#[test]
fn maintenance_names_fit_every_workspace_id() {
    let longest = WorkspaceId::new(&"w".repeat(WorkspaceId::MAX_LEN)).unwrap();
    let name = maintenance_name(&longest).unwrap();
    assert!(is_maintenance_name(name.as_str()));
    assert!(is_maintenance_name("m--acme"));
    assert!(!is_maintenance_name("acme"));
    assert!(!is_maintenance_name("m--"));
    assert!(!is_maintenance_name("m---a"));
}

async fn hold_a_loan(
    w: &Workspaces,
    id: &WorkspaceId,
    sandbox: &SandboxName,
) -> std::convert::Infallible {
    w.borrow(id, sandbox).unwrap();
    let _loan = Loan {
        registry: &w.registry,
        id,
    };
    match std::future::pending::<std::convert::Infallible>().await {}
}

#[tokio::test]
async fn a_dropped_request_gives_the_maintenance_loan_back() {
    let w = Workspaces::new(WorkspaceConfig::default());
    let id = WorkspaceId::new("acme").unwrap();
    let sandbox = maintenance_name(&id).unwrap();
    // What `delete` and `maintenance` do around a check that outlives the request.
    let held = hold_a_loan(&w, &id, &sandbox);
    let timed_out = tokio::time::timeout(Duration::from_millis(20), held).await;
    assert!(timed_out.is_err(), "the check never ends");
    assert!(
        w.registry.holder(&id).is_none(),
        "the workspace is still borrowed after the request was dropped"
    );
    w.borrow(&id, &sandbox).unwrap();
}
