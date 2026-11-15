// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn host(s: &str) -> HostName {
    HostName::new(s).unwrap()
}

#[test]
fn round_trips_through_json() {
    let spec = SourceSpec::GitCredential {
        host: host("dev.azure.com"),
        path: UrlPath::new("org").unwrap(),
        username: None,
    };
    let json = serde_json::to_string(&spec).unwrap();
    assert_eq!(
        json,
        r#"{"kind":"git_credential","host":"dev.azure.com","path":"org"}"#
    );
    assert_eq!(serde_json::from_str::<SourceSpec>(&json).unwrap(), spec);
}

#[test]
fn there_is_no_command_line_variant() {
    for bad in [
        r#"{"kind":"command","command":"curl evil"}"#,
        r#"{"kind":"gh","host":"github.com","account":"a b"}"#,
        r#"{"kind":"git_credential","host":"github.com","path":"../x"}"#,
    ] {
        assert!(serde_json::from_str::<SourceSpec>(bad).is_err(), "{bad}");
    }
}

fn az(tenant: &str, org: Option<&str>) -> SourceSpec {
    SourceSpec::AzCli {
        tenant: TenantId::new(tenant).unwrap(),
        org: org.map(|o| OrgName::new(o).unwrap()),
    }
}

#[test]
fn az_cli_has_no_command_field() {
    let spec = az("contoso.onmicrosoft.com", Some("acme"));
    let json = serde_json::to_string(&spec).unwrap();
    assert_eq!(
        json,
        r#"{"kind":"az_cli","tenant":"contoso.onmicrosoft.com","org":"acme"}"#
    );
    assert_eq!(serde_json::from_str::<SourceSpec>(&json).unwrap(), spec);
    // A field nobody defined is dropped, never carried: the spec cannot hold a command.
    let sneaky = r#"{"kind":"az_cli","tenant":"t1","command":"curl evil","resource":"x"}"#;
    let parsed = serde_json::from_str::<SourceSpec>(sneaky).unwrap();
    assert_eq!(
        serde_json::to_string(&parsed).unwrap(),
        r#"{"kind":"az_cli","tenant":"t1"}"#
    );
    for bad in [
        r#"{"kind":"az_cli"}"#,
        r#"{"kind":"az_cli","tenant":"--use-device-code"}"#,
        r#"{"kind":"az_cli","tenant":"a b"}"#,
        r#"{"kind":"az_cli","tenant":"t1","org":"a b"}"#,
    ] {
        assert!(serde_json::from_str::<SourceSpec>(bad).is_err(), "{bad}");
    }
}

#[test]
fn az_cli_is_for_azure_devops_only() {
    let scope = az("t1", Some("acme")).scope();
    assert_eq!(scope.host.as_str(), AZURE_DEVOPS_HOST);
    assert_eq!(scope.org.unwrap().as_str(), "acme");
    assert_eq!(az("t1", None).scope().org, None);
    assert_eq!(az("t1", None).describe(), "Azure CLI account in tenant t1");
    assert_eq!(
        az("t1", Some("acme")).describe(),
        "Azure CLI account in tenant t1 for acme"
    );
}

#[test]
fn each_az_failure_has_its_own_exact_step() {
    use crate::error::Tool;
    let spec = az("t1", Some("acme"));
    let steps: Vec<String> = [
        SourceError::ToolMissing(Tool::Az),
        SourceError::NotSignedIn,
        SourceError::WrongTenant,
        SourceError::NoAccess,
    ]
    .iter()
    .map(|e| spec.fix_step(e).unwrap())
    .collect();
    assert!(steps[0].contains("Install the Azure CLI"));
    assert!(steps[1].contains("`az login --tenant t1`"));
    assert!(steps[2].contains("not in tenant t1"));
    assert!(steps[3].contains("organisation acme"));
    for (i, a) in steps.iter().enumerate() {
        assert!(steps.iter().skip(i + 1).all(|b| a != b));
    }
    assert!(
        az("t1", None)
            .fix_step(&SourceError::NoAccess)
            .unwrap()
            .contains("an organisation administrator")
    );
    assert!(
        spec.fix_step(&SourceError::Timeout(Tool::Az))
            .unwrap()
            .contains("az account show")
    );
    assert_eq!(spec.fix_step(&SourceError::StoreUnavailable), None);
    assert_eq!(gh_spec().fix_step(&SourceError::NotSignedIn), None);
}

fn gh_spec() -> SourceSpec {
    SourceSpec::Gh {
        host: host("github.com"),
        account: AccountName::new("me").unwrap(),
    }
}

#[test]
fn describe_and_scope_name_the_target() {
    let gh = SourceSpec::Gh {
        host: host("github.com"),
        account: AccountName::new("me").unwrap(),
    };
    assert_eq!(gh.describe(), "gh account me on github.com");
    assert_eq!(gh.scope().org, None);
    let gc = SourceSpec::GitCredential {
        host: host("github.com"),
        path: UrlPath::new("o/r").unwrap(),
        username: Some(AccountName::new("me").unwrap()),
    };
    assert_eq!(
        gc.describe(),
        "Git credential for https://github.com/o/r (account me)"
    );
    let st = SourceSpec::Stored {
        id: StoredId::new("x1").unwrap(),
        scope: TokenScope {
            host: host("dev.azure.com"),
            org: Some(OrgName::new("acme").unwrap()),
        },
    };
    assert_eq!(st.describe(), "stored token x1 for dev.azure.com");
    assert_eq!(st.scope().org.unwrap().as_str(), "acme");
    let plain = SourceSpec::GitCredential {
        host: host("h.io"),
        path: UrlPath::new("o").unwrap(),
        username: None,
    };
    assert_eq!(plain.describe(), "Git credential for https://h.io/o");
}
