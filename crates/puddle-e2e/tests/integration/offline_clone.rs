// SPDX-License-Identifier: AGPL-3.0-or-later
//! A guest `git clone` while the route to the Git host is gone (QA playbook 07, S16 and S17): the
//! real `git` through the real agent and proxy to a fake Git host behind a link a test can cut.
//! The clone fails, the proxy's own 502 text says it could not connect and names the host, and
//! once the route is back the same clone works with nothing restarted.
#![cfg(target_os = "linux")]

use puddle_e2e::lab::{GitHost, Versions, git_available};

use crate::rig::{GitRig, LinkShim, NAME};

#[tokio::test]
async fn a_guest_clone_while_the_route_is_cut_gets_the_proxy_502_and_works_after_the_route_returns()
{
    if !git_available() {
        return;
    }
    let host = GitHost::start(NAME, Versions::Http1).await;
    let link = LinkShim::start(host.addr());
    let rig = GitRig::behind(host, link.addr()).await;
    let dir = rig.work("w");
    let url = format!("https://{NAME}/acme/web.git");

    link.cut().await;
    let ran = rig.run(&dir, &["clone", "-q", &url, "web"]).await;
    assert!(!ran.ok, "a clone through a cut route passed");
    eprintln!("git said: {}", ran.stderr);
    // The text the guest's tool is given by the proxy: a 502 that names what it could not reach.
    let answer = rig
        .request(
            "GET",
            "/acme/web.git/info/refs?service=git-upload-pack",
            &[],
            b"",
        )
        .await;
    assert_eq!(answer.status, 502, "{}", answer.body);
    assert!(
        answer.body.contains("could not connect"),
        "the proxy's text does not say it could not connect: {}",
        answer.body
    );
    assert!(
        answer.body.contains(NAME),
        "the host is not named: {}",
        answer.body
    );

    // The route returns: the same clone works, no restart of anything.
    link.restore().await;
    let ran = rig.run(&dir, &["clone", "-q", &url, "web"]).await;
    assert!(
        ran.ok,
        "the clone after the route returned failed: {}",
        ran.stderr
    );
    assert!(dir.join("web/.git").is_dir());
}
