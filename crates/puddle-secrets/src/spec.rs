// SPDX-License-Identifier: GPL-3.0-or-later
//! The closed list of places a secret can come from.

use serde::{Deserialize, Serialize};

use crate::error::SourceError;
use crate::name::{AccountName, HostName, OrgName, StoredId, TenantId, UrlPath};

/// The host an Azure CLI token is for. `*.visualstudio.com` names are the same service.
pub const AZURE_DEVOPS_HOST: &str = "dev.azure.com";

/// Which host (and, for a token that belongs to one organisation, which organisation) a secret is
/// for. Sources do not enforce it: whoever uses the token (the proxy's injector, a host-side API
/// call) checks it against where the token is about to go.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TokenScope {
    /// The Git host.
    pub host: HostName,
    /// The organisation, for tokens that are per organisation (Azure DevOps).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org: Option<OrgName>,
}

/// One secret source. A closed enum: there is no variant that carries a command line, so no
/// configuration or API input can make Puddle run anything but the fixed invocations in this crate.
/// An identity owns one of these per credential; a source never assumes it is the only one for a
/// workspace.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SourceSpec {
    /// The token of a named, signed-in GitHub CLI account (`gh auth token --user`), never "whichever
    /// account is active".
    Gh {
        /// The GitHub host.
        host: HostName,
        /// The account.
        account: AccountName,
    },
    /// Git Credential Manager (or any configured Git credential helper), asked for one HTTPS URL
    /// with its path. The path is always sent.
    GitCredential {
        /// The host.
        host: HostName,
        /// The URL path that selects the credential.
        path: UrlPath,
        /// A GitHub account name, when the host has several.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        username: Option<AccountName>,
    },
    /// A token the user pasted, kept in the OS credential store under Puddle's own name.
    Stored {
        /// The id of the entry.
        id: StoredId,
        /// What the token is for.
        scope: TokenScope,
    },
    /// The Entra access token of the account signed in to the Azure CLI (`az`) for one tenant, for
    /// Azure DevOps only. The user signs in to `az` themselves; Puddle only reads a token. There is
    /// no resource, scope or command field: the resource is Azure DevOps' own application id.
    AzCli {
        /// The tenant to ask `az` for, never "whichever tenant is active".
        tenant: TenantId,
        /// The Azure DevOps organisation, when the token is meant for one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        org: Option<OrgName>,
    },
}

impl SourceSpec {
    /// What the credential is for, when the source knows.
    #[must_use]
    pub fn scope(&self) -> TokenScope {
        match self {
            Self::Gh { host, .. } | Self::GitCredential { host, .. } => TokenScope {
                host: host.clone(),
                org: None,
            },
            Self::Stored { scope, .. } => scope.clone(),
            Self::AzCli { org, .. } => TokenScope {
                host: azure_devops_host(),
                org: org.clone(),
            },
        }
    }

    /// A line for logs, events and errors. Names only, never a value.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Gh { host, account } => format!("gh account {account} on {host}"),
            Self::GitCredential {
                host,
                path,
                username: None,
            } => {
                format!("Git credential for https://{host}/{path}")
            }
            Self::GitCredential {
                host,
                path,
                username: Some(user),
            } => {
                format!("Git credential for https://{host}/{path} (account {user})")
            }
            Self::Stored { id, scope } => format!("stored token {id} for {}", scope.host),
            Self::AzCli { tenant, org: None } => format!("Azure CLI account in tenant {tenant}"),
            Self::AzCli {
                tenant,
                org: Some(org),
            } => format!("Azure CLI account in tenant {tenant} for {org}"),
        }
    }

    /// The exact step that fixes `err` for this source, when there is one the user can take
    /// (only the Azure CLI has any: nothing there starts a sign-in for the user).
    #[must_use]
    pub fn fix_step(&self, err: &SourceError) -> Option<String> {
        let Self::AzCli { tenant, org } = self else {
            return None;
        };
        let on = org
            .as_ref()
            .map_or_else(String::new, |o| format!(" for {o}"));
        match err {
            SourceError::ToolMissing(_) => Some(
                "Install the Azure CLI (https://aka.ms/installazurecli), then open a new terminal so `az` is on the PATH.".to_owned(),
            ),
            SourceError::NotSignedIn => Some(format!(
                "Run `az login --tenant {tenant}` in a terminal and finish the sign-in{on}."
            )),
            SourceError::WrongTenant => Some(format!(
                "The signed-in account is not in tenant {tenant}. Run `az login --tenant {tenant}` with an account that is a member of it."
            )),
            SourceError::NoAccess => Some(match org {
                Some(org) => format!(
                    "The account has no access to Azure DevOps in tenant {tenant}. Ask an administrator of the organisation {org} to add it, then try again."
                ),
                None => format!(
                    "The account has no access to Azure DevOps in tenant {tenant}. Ask an organisation administrator to add it, then try again."
                ),
            }),
            SourceError::Timeout(_) => Some(
                "`az` did not answer in time. Run `az account show` in a terminal to see whether it is waiting for something.".to_owned(),
            ),
            _ => None,
        }
    }
}

#[expect(
    clippy::expect_used,
    reason = "a constant that is a valid host name; the test below proves it"
)]
fn azure_devops_host() -> HostName {
    HostName::new(AZURE_DEVOPS_HOST).expect("a valid host name")
}

#[cfg(test)]
mod tests {
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
}
