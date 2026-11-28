// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[tokio::test]
async fn git_over_ssh_off_makes_no_host_keys_but_still_names_the_ssh_hosts_so_they_are_refused() {
    let store = store();
    let (injection, terminations) = injection(&store);
    let ws = workspace("alpha");
    attach(&store, &ws, "ada", "git.corp.example");
    let began = injection.begin(&ws, false).await.unwrap();
    assert!(began.ssh.is_none());
    let termination = terminations.termination(&ws).unwrap();
    assert!(termination.git_ssh_keys().is_none());
    assert!(
        termination
            .git_ssh_hosts()
            .contains(&Host::parse_normalised("git.corp.example").unwrap())
    );
}
#[tokio::test]
async fn each_start_makes_new_host_keys_the_workspace_serves_and_the_guest_lists() {
    let store = store();
    let (injection, terminations) = injection_with_ssh(&store);
    let (a, b) = (workspace("alpha"), workspace("beta"));
    attach(&store, &a, "ada", "git.corp.example");
    let first = injection.begin(&a, false).await.unwrap();
    let other = injection.begin(&b, false).await.unwrap();

    // The keys served are the ones listed, under the built-in names and the credential's.
    let listing = first.ssh.unwrap();
    let served = served_keys(&terminations, &a);
    assert_eq!(served.len(), 3);
    let lines = listing.lines().unwrap();
    for key in &served {
        let half = key.split(' ').take(2).collect::<Vec<_>>().join(" ");
        for name in ["github.com", "ssh.dev.azure.com", "git.corp.example"] {
            assert!(lines.contains(&format!("{name} {half}")), "{name}");
            assert!(lines.contains(&format!("[{name}]:443 {half}")), "{name}");
        }
    }
    let ssh_hosts = terminations.termination(&a).unwrap();
    assert!(
        ssh_hosts
            .git_ssh_hosts()
            .contains(&Host::parse_normalised("git.corp.example").unwrap())
    );
    // Another workspace has other keys, and a restart of the first makes new ones.
    assert_ne!(served_keys(&terminations, &b), served);
    assert!(other.ssh.is_some());
    injection.end(&a);
    injection.begin(&a, false).await.unwrap();
    assert_ne!(served_keys(&terminations, &a), served);
}
#[tokio::test]
async fn a_change_to_the_identities_keeps_the_keys_and_changes_the_names() {
    let store = store();
    let (injection, terminations) = injection_with_ssh(&store);
    let ws = workspace("alpha");
    let began = injection.begin(&ws, false).await.unwrap();
    let before = served_keys(&terminations, &ws);
    assert!(
        !began
            .ssh
            .unwrap()
            .lines()
            .unwrap()
            .contains("git.corp.example")
    );
    attach(&store, &ws, "ada", "git.corp.example");
    injection.resync(&ws).unwrap();
    assert_eq!(served_keys(&terminations, &ws), before);
    assert!(
        terminations
            .termination(&ws)
            .unwrap()
            .git_ssh_hosts()
            .contains(&Host::parse_normalised("git.corp.example").unwrap())
    );
}
