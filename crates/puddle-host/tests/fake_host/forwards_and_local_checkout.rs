// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pinned forwards and starting from a local checkout.

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_pinned_forward_follows_the_workspace_through_stop_start_and_delete() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    // A port of this computer nobody has right now: a quiet one, since a port 0 port given up is free
    // for any parallel test's port 0 or source port before the host binds it.
    let port = puddle_test_kit::quiet_port();
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let ports = |reply: support::Reply| reply.json();

    let none = ports(api.get("/api/workspaces/acme/ports").await);
    assert_eq!(none["running"], true);
    assert_eq!(none["ports"], json!([]));

    let body = json!({"host_port": port, "pinned": true}).to_string();
    let put = api.put("/api/workspaces/acme/ports/3000", &body).await;
    assert_eq!(put.status, 200, "{}", put.body);
    assert_eq!(
        put.json(),
        json!({"guest_port": 3000, "listening": false, "forwarded": true, "pinned": true, "host_port": port, "problem": null})
    );
    // It listens on loopback and is one of Puddle's own listeners: no guest may reach it.
    assert!(tokio::net::TcpStream::connect(addr).await.is_ok());
    assert_eq!(
        host.endpoints().find(addr, &puddle_netpolicy::OwnAddresses),
        Some(EndpointKind::PinnedForward)
    );
    // A second workspace-less request for the same port of this computer says who has it.
    let clash = api
        .put(
            "/api/workspaces/acme/ports/4000",
            &json!({"host_port": port}).to_string(),
        )
        .await;
    assert_eq!(clash.status, 409, "{}", clash.body);
    assert!(clash.body.contains("already in use by"), "{}", clash.body);

    // Stopping closes the forward and keeps the pin.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(tokio::net::TcpStream::connect(addr).await.is_err());
    assert_eq!(
        host.endpoints().find(addr, &puddle_netpolicy::OwnAddresses),
        None
    );
    let stopped = ports(api.get("/api/workspaces/acme/ports").await);
    assert_eq!(stopped["running"], false);
    assert_eq!(
        stopped["ports"],
        json!([{"guest_port": 3000, "listening": false, "forwarded": false, "pinned": true, "host_port": port, "problem": null}])
    );
    let refused = api.put("/api/workspaces/acme/ports/3000", &body).await;
    assert_eq!(refused.status, 409, "{}", refused.body);

    // The next start puts it back on the same port.
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let back = ports(api.get("/api/workspaces/acme/ports").await);
    assert_eq!(back["running"], true);
    assert_eq!(back["ports"][0]["forwarded"], true);
    assert_eq!(back["ports"][0]["host_port"], port);
    assert!(tokio::net::TcpStream::connect(addr).await.is_ok());

    // Stopping the forward by hand closes it and takes the pin with it; pinning again restores both.
    let released = api.delete("/api/workspaces/acme/ports/3000", None).await;
    assert_eq!(released.status, 204, "{}", released.body);
    assert!(tokio::net::TcpStream::connect(addr).await.is_err());
    assert_eq!(
        host.store()
            .pinned_ports(&WorkspaceId::new("acme").unwrap())
            .unwrap(),
        []
    );
    let again = api.put("/api/workspaces/acme/ports/3000", &body).await;
    assert_eq!(again.status, 200, "{}", again.body);

    // Deleting the workspace removes its pins with it.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let name = WorkspaceId::new("acme").unwrap();
    assert_eq!(host.store().pinned_ports(&name).unwrap().len(), 1);
    let delete = api
        .delete(
            "/api/workspaces/acme",
            Some(&json!({"confirm": true}).to_string()),
        )
        .await;
    assert_eq!(delete.status, 202, "{}", delete.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(host.store().pinned_ports(&name).unwrap(), []);
    host.shutdown().await;
}

/// A checkout on this computer by its files alone (Puddle never runs git against it): a `.git`
/// whose config names `origin` as an SSH remote of GitHub and asks for a file-system monitor, a
/// hook, and an `.env` beside it. The monitor and the hook would leave `ran` next to the folder if
/// anything ran them on this computer.
fn local_checkout(base: &std::path::Path, name: &str) -> PathBuf {
    let work = base.join(name);
    let git = work.join(".git");
    for dir in ["objects", "refs/heads", "hooks"] {
        std::fs::create_dir_all(git.join(dir)).unwrap();
    }
    let ran = base.join("ran");
    std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(
        git.join("config"),
        format!(
            "[core]\n\tfsmonitor = touch {ran}\n[remote \"origin\"]\n\turl = git@github.com:acme/{name}.git\n",
            ran = ran.display()
        ),
    )
    .unwrap();
    std::fs::write(
        git.join("hooks").join("post-checkout"),
        format!("#!/bin/sh\ntouch {}\n", ran.display()),
    )
    .unwrap();
    std::fs::write(work.join(".env"), "SECRET=never-in-a-guest\n").unwrap();
    work
}

/// Every file below `dir` with its bytes and modification time.
fn files_of(dir: &std::path::Path) -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                todo.push(p);
            } else {
                let modified = std::fs::metadata(&p).unwrap().modified().unwrap();
                out.push((p.clone(), std::fs::read(&p).unwrap(), modified));
            }
        }
    }
    out.sort();
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_starts_from_a_checkout_on_this_computer_and_the_checkout_is_left_as_it_was() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let tmp = support::scratch_dir();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let work = local_checkout(&base, "shop");
    let before = files_of(&base);
    let mut events = api.events().await;

    let body =
        json!({"name": "local", "repositories": [{"local_path": work.display().to_string()}]});
    let reply = api.post("/api/workspaces", &body.to_string()).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    // The origin's HTTPS form is the repository's address, the folder's name its directory.
    let entry = &reply.json()["repositories"][0];
    assert_eq!(entry["url"], "https://github.com/acme/shop.git", "{entry}");
    assert_eq!(entry["directory"], "shop", "{entry}");
    let end = events.until(ended("local"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");

    // The import ran first, in the maintenance sandbox, and the workspace's own boot cloned nothing.
    let commands = rig.guest.commands();
    assert_eq!(
        commands[..3],
        ["m--local import", "m--local finish-import", "m--local sync"],
        "{commands:#?}"
    );
    assert!(
        commands[3..]
            .iter()
            .all(|c| c.starts_with("local ") && !c.ends_with(" clone")),
        "{commands:#?}"
    );
    let imports = rig.guest.imports.lock().unwrap().clone();
    assert_eq!(imports.len(), 1);
    let (args, head, tree_visible, hooks) = &imports[0];
    assert_eq!(
        head, "ref: refs/heads/main\n",
        "the import read the checkout's own .git"
    );
    assert!(!tree_visible, "the working tree was visible to the import");
    assert_eq!(
        hooks.as_deref(),
        Some("/dev/null"),
        "no hook may run in the import"
    );
    assert_eq!(args.last().unwrap(), "/workspaces/local/shop");
    // The import sandbox had no route and is gone; the workspace's has its own.
    let names: Vec<String> = rig
        .runtime
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["local"]);

    // The listing ties the clone to its entry, by the origin it was given.
    let list = checkouts_of(&api, "local").await;
    assert_eq!(
        list["checkouts"][0]["entry"]["url"],
        "https://github.com/acme/shop.git"
    );

    // Nothing on this computer ran or changed: no hook, no monitor, every file as it was.
    assert!(!base.join("ran").exists());
    assert_eq!(files_of(&base), before);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_that_cannot_be_started_from_is_refused_before_anything_exists() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let tmp = support::scratch_dir();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let plain = base.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let linked = base.join("linked");
    std::fs::create_dir_all(&linked).unwrap();
    std::fs::write(linked.join(".git"), "gitdir: /elsewhere/.git/worktrees/x\n").unwrap();
    let no_origin = local_checkout(&base, "no-origin");
    std::fs::write(no_origin.join(".git").join("config"), "[core]\n").unwrap();
    let unknown = local_checkout(&base, "unknown");
    std::fs::write(
        unknown.join(".git").join("config"),
        "[remote \"origin\"]\n\turl = git@git.example.com:acme/x.git\n",
    )
    .unwrap();

    for (folder, want) in [
        (&plain, "has no .git"),
        (&linked, "linked worktree or a submodule"),
        (&no_origin, "has no origin remote"),
        (&unknown, "knows no HTTPS form"),
    ] {
        let body = json!({"name": "refused", "repositories": [{"local_path": folder.display().to_string()}]});
        let reply = api.post("/api/workspaces", &body.to_string()).await;
        assert_eq!(reply.status, 422, "{}: {}", folder.display(), reply.body);
        assert!(reply.body.contains(want), "{want}: {}", reply.body);
    }
    assert!(rig.guest.commands().is_empty());
    assert!(
        api.get("/api/workspaces").await.json()["workspaces"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // An address given with the folder stands in for a missing or unusable origin.
    let mut events = api.events().await;
    let body = json!({"name": "given", "repositories": [{"local_path": unknown.display().to_string(), "url": "https://git.example.com/acme/x.git"}]});
    let reply = api.post("/api/workspaces", &body.to_string()).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    assert_eq!(
        reply.json()["repositories"][0]["url"],
        "https://git.example.com/acme/x.git"
    );
    events.until(ended("given"), Duration::from_secs(20)).await;
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_checkout_git_refuses_is_a_problem_and_a_boot_that_fails_after_the_import_leaves_no_volume()
 {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let tmp = support::scratch_dir();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let work = local_checkout(&base, "shop");
    let body = |name: &str| {
        json!({"name": name, "repositories": [{"local_path": work.display().to_string()}]})
            .to_string()
    };

    // git refuses the checkout: the workspace is made without it, and a problem says what to do.
    rig.guest.fail_import.store(true, Ordering::SeqCst);
    let mut events = api.events().await;
    let reply = api.post("/api/workspaces", &body("refused")).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events
        .until(ended("refused"), Duration::from_secs(20))
        .await;
    assert_eq!(end["step"], "done", "{end}");
    let problems = api.get("/api/problems").await.json();
    let shop = entry_of(&api, "refused", "shop").await;
    let problem = problems["problems"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["key"] == format!("clone:refused:{shop}"))
        .unwrap_or_else(|| panic!("{problems}"))
        .clone();
    assert!(
        problem["title"]
            .as_str()
            .unwrap()
            .contains("https://github.com/acme/shop.git"),
        "{problem}"
    );
    let detail = problem["detail"].as_str().unwrap();
    assert!(
        detail.starts_with("git exited 128: fatal: bad object HEAD"),
        "{problem}"
    );
    assert!(
        detail.contains("add the repository by its address on the Git tab"),
        "{problem}"
    );
    rig.guest.fail_import.store(false, Ordering::SeqCst);

    // The import made the volume and then the boot failed: the volume goes with the create.
    rig.guest.fail_boot.store(true, Ordering::SeqCst);
    let reply = api.post("/api/workspaces", &body("unbooted")).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events
        .until(ended("unbooted"), Duration::from_secs(20))
        .await;
    assert_eq!(end["step"], "failed", "{end}");
    let volumes: Vec<String> = rig
        .runtime
        .list_volumes()
        .await
        .unwrap()
        .into_iter()
        .map(|v| v.name)
        .collect();
    assert!(!volumes.contains(&"ws-unbooted".to_owned()), "{volumes:?}");
    assert!(
        rig.guest
            .commands()
            .contains(&"m--unbooted import".to_owned())
    );

    // The volume cannot be removed either: the failure says so and names the volume left.
    rig.runtime.inject(
        Op::RemoveVolume,
        Fault::once(puddle_compute::ComputeError::Runtime {
            op: "remove_volume",
            message: "the disk is busy".into(),
        }),
    );
    let reply = api.post("/api/workspaces", &body("stuck")).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events.until(ended("stuck"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    assert!(
        end.to_string()
            .contains("the new volume ws-stuck was not removed"),
        "{end}"
    );
    host.shutdown().await;
}
