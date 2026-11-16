// SPDX-License-Identifier: AGPL-3.0-or-later
//! Editor and capture problems on a workspace.

use super::*;

/// Speaks to the host as `name`'s guest agent would: one session on the workspace's route, one
/// control stream with `lines` on it. The session stays open until the returned guard drops.
async fn agent_says(
    rig: &Rig,
    name: &str,
    lines: &[puddle_agent_proto::AgentMessage],
) -> tokio::task::JoinHandle<()> {
    use futures_util::StreamExt as _;
    use puddle_agent_proto::tokio_yamux::Session;
    use tokio::io::AsyncWriteExt as _;
    let sandbox = WorkspaceId::new(name).unwrap().sandbox_name();
    let routes = rig.runtime.routes(&sandbox);
    let route = routes.first().unwrap();
    let conn = puddle_ipc::connect(&route.host).await.unwrap();
    let mut session = Session::new_client(conn, puddle_agent_proto::yamux::client_config());
    let mut control = session.control();
    let driver = tokio::spawn(async move { while let Some(Ok(_)) = session.next().await {} });
    let mut stream = control.open_stream().await.unwrap();
    stream
        .write_all(puddle_agent_proto::control::PREAMBLE)
        .await
        .unwrap();
    for line in lines {
        stream.write_all(&line.to_line().unwrap()).await.unwrap();
    }
    stream.flush().await.unwrap();
    driver
}

#[tokio::test(flavor = "multi_thread")]
async fn an_entrypoint_that_dies_later_and_failing_lookups_are_problems_the_guest_cannot_widen() {
    use puddle_agent_proto::{AgentMessage, StubDnsTrouble};
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    create(&api, &mut events, "other").await;

    let session = agent_says(
        &rig,
        "acme",
        &[
            AgentMessage::entrypoint_exited(1, "dockerd: failed to start".to_owned()),
            AgentMessage::entrypoint_exited(2, "a second, different story".to_owned()),
            AgentMessage::stub_dns(StubDnsTrouble::UdpDropped, 40),
            AgentMessage::stub_dns(StubDnsTrouble::LookupTimedOut, 3),
        ],
    )
    .await;
    let problem = wait_for_problem(&host, "stub-dns:acme").await;
    assert_eq!(problem.title, "Name lookups in acme are failing");
    let entry = wait_for_problem(&host, "entrypoint:acme").await;
    assert_eq!(entry.title, "The image's entrypoint in acme stopped");
    assert!(
        entry.detail.contains("status 1: dockerd: failed to start"),
        "{}",
        entry.detail
    );
    assert!(
        host.problems()
            .list()
            .iter()
            .all(|p| p.workspace.as_ref().map(WorkspaceId::as_str) == Some("acme")),
        "{:?}",
        host.problems().list()
    );

    // A stop ends the DNS problem with the machine; the entrypoint problem stays until a start.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let keys: Vec<_> = host.problems().list().into_iter().map(|p| p.key).collect();
    assert_eq!(keys, ["entrypoint:acme"]);
    session.abort();
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(
        host.problems().list().is_empty(),
        "{:?}",
        host.problems().list()
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_editor_download_is_a_problem_on_its_workspace_until_an_install_works_or_it_stops()
 {
    use puddle_agent_proto::{AgentMessage, EditorProblemKind};
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    create(&api, &mut events, "other").await;

    let session = agent_says(
        &rig,
        "acme",
        &[AgentMessage::editor_status(
            EditorProblemKind::Install,
            Some("the download does not match the checksum Microsoft published for it"),
        )],
    )
    .await;
    let (title, detail) = problem_is(&api, "editor-install:acme", true).await.unwrap();
    assert_eq!(
        title,
        "The browser editor could not be installed in acme: the download does not match the checksum Microsoft published for it"
    );
    assert!(
        detail.contains("desktop VS Code are not affected"),
        "{detail}"
    );
    assert!(
        problem_is(&api, "editor-install:other", false)
            .await
            .is_none()
    );

    // The install worked on a later try: the agent says so and the problem ends.
    let fixed = agent_says(
        &rig,
        "acme",
        &[AgentMessage::editor_status(
            EditorProblemKind::Install,
            None,
        )],
    )
    .await;
    assert!(
        problem_is(&api, "editor-install:acme", false)
            .await
            .is_none()
    );
    fixed.abort();

    // A stop ends it too: the machine it is about is gone.
    let again = agent_says(
        &rig,
        "acme",
        &[AgentMessage::editor_status(
            EditorProblemKind::Install,
            Some("refused again"),
        )],
    )
    .await;
    problem_is(&api, "editor-install:acme", true).await.unwrap();
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(
        problem_is(&api, "editor-install:acme", false)
            .await
            .is_none()
    );
    again.abort();
    session.abort();
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_newer_editor_build_that_was_refused_is_a_problem_beside_the_install_one_until_a_look_works()
 {
    use puddle_agent_proto::{AgentMessage, EditorProblemKind};
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;

    let session = agent_says(
        &rig,
        "acme",
        &[AgentMessage::editor_status(
            EditorProblemKind::Update,
            Some("a newer build was refused: the download does not match the checksum Microsoft published for it"),
        )],
    )
    .await;
    let (title, detail) = problem_is(&api, "editor-update:acme", true).await.unwrap();
    assert_eq!(
        title,
        "The browser editor in acme could not update: a newer build was refused: the download does not match the checksum Microsoft published for it"
    );
    assert!(
        detail.contains("keeps working on the build it has"),
        "{detail}"
    );
    assert!(
        problem_is(&api, "editor-install:acme", false)
            .await
            .is_none(),
        "the editor itself is installed"
    );

    // A later look worked: the agent says so and the problem ends.
    let fixed = agent_says(
        &rig,
        "acme",
        &[AgentMessage::editor_status(EditorProblemKind::Update, None)],
    )
    .await;
    assert!(
        problem_is(&api, "editor-update:acme", false)
            .await
            .is_none()
    );
    fixed.abort();
    session.abort();
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_editor_server_that_runs_as_root_for_want_of_its_user_is_a_problem_until_a_start_finds_the_user()
 {
    use puddle_agent_proto::{AgentMessage, EditorProblemKind};
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;

    let session = agent_says(
        &rig,
        "acme",
        &[AgentMessage::editor_status(
            EditorProblemKind::SessionUser,
            Some("its user agent is missing in the guest"),
        )],
    )
    .await;
    let (title, detail) = problem_is(&api, "editor-session-user:acme", true)
        .await
        .unwrap();
    assert_eq!(
        title,
        "The browser editor in acme runs as root: its user agent is missing in the guest"
    );
    assert!(detail.contains("belong to root"), "{detail}");
    assert!(
        detail.contains("switch the workspace's session user to root"),
        "{detail}"
    );

    let fixed = agent_says(
        &rig,
        "acme",
        &[AgentMessage::editor_status(
            EditorProblemKind::SessionUser,
            None,
        )],
    )
    .await;
    assert!(
        problem_is(&api, "editor-session-user:acme", false)
            .await
            .is_none()
    );
    fixed.abort();
    session.abort();
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn capture_rules_the_guest_tore_down_or_that_cannot_install_are_problems_until_they_are_fixed_or_it_stops()
 {
    use puddle_agent_proto::AgentMessage;
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    create(&api, &mut events, "other").await;

    let session = agent_says(
        &rig,
        "acme",
        &[
            AgentMessage::capture_tampered(true),
            AgentMessage::capture_tampered(false),
            AgentMessage::capture_tampered(true),
            AgentMessage::capture_tampered(true),
            AgentMessage::capture_unavailable("nft: Operation not supported".to_owned()),
        ],
    )
    .await;
    // The agent's messages are handled in order: with the last one's problem up, the count is final.
    let (title, detail) = problem_is(&api, "capture-unavailable:acme", true)
        .await
        .unwrap();
    assert_eq!(
        title,
        "Tools in acme that ignore proxy settings are offline: nft: Operation not supported"
    );
    assert!(detail.contains("Puddle keeps trying"), "{detail}");
    let (title, detail) = problem_is(&api, "capture-tamper:acme", true).await.unwrap();
    assert_eq!(
        title,
        "Something in acme deleted Puddle's capture rules; Puddle put them back"
    );
    assert!(detail.contains("nft flush ruleset"), "{detail}");
    assert!(
        problem_is(&api, "capture-tamper:other", false)
            .await
            .is_none()
    );

    // The rules went in on a later try: the agent says so; the tamper notice stays.
    let fixed = agent_says(&rig, "acme", &[AgentMessage::capture_ready()]).await;
    assert!(
        problem_is(&api, "capture-unavailable:acme", false)
            .await
            .is_none()
    );
    problem_is(&api, "capture-tamper:acme", true).await.unwrap();
    fixed.abort();

    // A stop ends the tamper notice with the machine it is about.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(
        problem_is(&api, "capture-tamper:acme", false)
            .await
            .is_none()
    );
    session.abort();
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_that_fails_leaves_the_workspace_running_and_its_reports_still_count() {
    use puddle_agent_proto::{AgentMessage, StubDnsTrouble};
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    rig.runtime
        .inject(Op::Stop, Fault::once(disk_error("stop")).only_for("acme"));
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;

    let session = agent_says(
        &rig,
        "acme",
        &[AgentMessage::stub_dns(StubDnsTrouble::LookupFailed, 2)],
    )
    .await;
    let problem = wait_for_problem(&host, "stub-dns:acme").await;
    assert_eq!(problem.title, "Name lookups in acme are failing");
    session.abort();
    host.shutdown().await;
}
