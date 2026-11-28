// SPDX-License-Identifier: AGPL-3.0-or-later
//! Decisions about Azure DevOps credentials and host names.

use super::*;

// Azure DevOps.

#[tokio::test]
async fn azure_devops_takes_a_pat_as_basic_an_entra_token_as_a_bearer_and_every_host_name_of_the_org()
 {
    let w = World::new();
    w.identity("Contoso", "dev.azure.com", &["contoso"], false, "ADO-PAT");
    w.identity(
        "Fabrikam",
        "dev.azure.com",
        &["fabrikam"],
        false,
        "eyJhbGciOiJSUzI1NiJ9.eyJhIjoxfQ.c2ln",
    );
    w.switches(OnlyListed::Neither);
    let pat = w
        .decide(
            "dev.azure.com",
            "GET",
            "/contoso/proj/_git/repo/info/refs?service=git-upload-pack",
            &[],
        )
        .await;
    assert!(injected(&pat, &basic("", "ADO-PAT")), "{pat:?}");
    let entra = w
        .decide(
            "dev.azure.com",
            "POST",
            "/fabrikam/_git/repo/git-upload-pack",
            &[],
        )
        .await;
    assert!(
        injected(&entra, "Bearer eyJhbGciOiJSUzI1NiJ9.eyJhIjoxfQ.c2ln"),
        "{entra:?}"
    );
    // The same organisation by its other name.
    let old = w
        .decide(
            "contoso.visualstudio.com",
            "GET",
            "/proj/_git/repo/info/refs?service=git-upload-pack",
            &[],
        )
        .await;
    assert!(injected(&old, &basic("", "ADO-PAT")), "{old:?}");
    let elsewhere = w
        .decide(
            "other.visualstudio.com",
            "GET",
            "/proj/_git/repo/info/refs",
            &[],
        )
        .await;
    assert!(matches!(elsewhere, InjectDecision::PassThroughNoting(_)));
}

// A host with two names.

#[tokio::test]
async fn an_identity_bound_to_the_name_asked_for_covers_an_azure_devops_host_alias() {
    let w = World::new();
    w.identity(
        "Old",
        "contoso.visualstudio.com",
        &["contoso"],
        false,
        "ADO-OLD",
    );
    w.switches(OnlyListed::Neither);
    let decision = w
        .decide(
            "contoso.visualstudio.com",
            "GET",
            "/proj/_git/repo/info/refs?service=git-upload-pack",
            &[],
        )
        .await;
    assert!(injected(&decision, &basic("", "ADO-OLD")), "{decision:?}");
}
