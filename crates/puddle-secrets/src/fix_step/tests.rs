// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_types::{AccountName, HostName, OrgName, SourceSpec, TenantId};

use super::*;

fn az(tenant: &str, org: Option<&str>) -> SourceSpec {
    SourceSpec::AzCli {
        tenant: TenantId::new(tenant).unwrap(),
        org: org.map(|o| OrgName::new(o).unwrap()),
    }
}

fn gh_spec() -> SourceSpec {
    SourceSpec::Gh {
        host: HostName::new("github.com").unwrap(),
        account: AccountName::new("me").unwrap(),
    }
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
    .map(|e| fix_step(&spec, e).unwrap())
    .collect();
    assert!(steps[0].contains("Install the Azure CLI"));
    assert!(steps[1].contains("`az login --tenant t1`"));
    assert!(steps[2].contains("not in tenant t1"));
    assert!(steps[3].contains("organisation acme"));
    for (i, a) in steps.iter().enumerate() {
        assert!(steps.iter().skip(i + 1).all(|b| a != b));
    }
    assert!(
        fix_step(&az("t1", None), &SourceError::NoAccess)
            .unwrap()
            .contains("an organisation administrator")
    );
    assert!(
        fix_step(&spec, &SourceError::Timeout(Tool::Az))
            .unwrap()
            .contains("az account show")
    );
    assert_eq!(fix_step(&spec, &SourceError::StoreUnavailable), None);
    assert_eq!(fix_step(&gh_spec(), &SourceError::NotSignedIn), None);
}
