// SPDX-License-Identifier: AGPL-3.0-or-later
//! The step a user can take to fix a failed source.

use puddle_types::SourceSpec;

use crate::error::SourceError;

/// The exact step that fixes `err` for this source, when there is one the user can take
/// (only the Azure CLI has any: nothing there starts a sign-in for the user).
#[must_use]
pub fn fix_step(source: &SourceSpec, err: &SourceError) -> Option<String> {
    let SourceSpec::AzCli { tenant, org } = source else {
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

#[cfg(test)]
mod tests;
