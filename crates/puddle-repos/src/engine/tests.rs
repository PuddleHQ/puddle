// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_secrets::{AccountName, HostName, OrgName, StoredId, TokenScope, UrlPath};

use super::*;

#[test]
fn a_sources_own_organisation_is_the_pasted_tokens_or_the_git_credentials_path_and_no_other_has_one()
 {
    let host = HostName::new("dev.azure.com").unwrap();
    let stored = SourceSpec::Stored {
        id: StoredId::new("tok-1").unwrap(),
        scope: TokenScope {
            host: host.clone(),
            org: Some(OrgName::new("Acme").unwrap()),
        },
    };
    let git = SourceSpec::GitCredential {
        host: host.clone(),
        path: UrlPath::new("Contoso/Fabrikam").unwrap(),
        username: None,
    };
    let gh = SourceSpec::Gh {
        host,
        account: AccountName::new("me").unwrap(),
    };
    assert_eq!(source_organisation(&stored).as_deref(), Some("acme"));
    assert_eq!(source_organisation(&git).as_deref(), Some("contoso"));
    assert_eq!(source_organisation(&gh), None);
}
