// SPDX-License-Identifier: GPL-3.0-or-later
//! A workspace request the API drops (its time limit, a client that went away) stops at one of
//! its awaits. Wherever that is, the workspace must not stay busy and no maintenance sandbox may
//! be left running: each request here is dropped at every await it has, one after the other.

use std::pin::pin;
use std::task::Poll;

use futures_util::future::BoxFuture;
use puddle_api::{
    AddRepository, DisplayName, GitSetup, NewRepository, NewWorkspace, RepoUrl, Repository,
};
use puddle_types::{EntryId, WorkspaceStatus};

use super::*;

/// A [`GitSetup`] whose setup waits once, as the real one waits for the store, so a create or a
/// clone can be dropped there. It records the new workspace's user in the store, as the real one
/// does.
struct GitThatWaits<'a>(&'a puddle_store::Store);

impl GitSetup for GitThatWaits<'_> {
    fn set_up<'a>(
        &'a self,
        _workspace: &'a WorkspaceId,
        _repositories: &'a [NewRepository],
    ) -> BoxFuture<'a, ()> {
        Box::pin(tokio::task::yield_now())
    }

    fn claim(
        &self,
        workspace: &WorkspaceId,
        owner: puddle_store::UserId,
    ) -> Result<(), puddle_api::WorkspaceError> {
        self.0
            .set_workspace_owner(workspace, owner)
            .map_err(|err| puddle_api::WorkspaceError::Unavailable(err.to_string()))
    }
}

/// Polls `request` until it has been pending `waits` times, then drops it. True when it ended
/// first: there is no later await to drop it at.
async fn drop_after<F: Future>(request: F, waits: usize) -> bool {
    let mut request = pin!(request);
    let mut pending = 0;
    std::future::poll_fn(|cx| match request.as_mut().poll(cx) {
        Poll::Ready(_) => Poll::Ready(true),
        Poll::Pending if pending == waits => Poll::Ready(false),
        Poll::Pending => {
            pending += 1;
            Poll::Pending
        }
    })
    .await
}

/// Waits until no workspace is busy and no maintenance sandbox runs.
async fn settled(host: &Host<FakeRuntime>, rig: &Rig, what: &str) {
    puddle_test_kit::eventually_async(what, Duration::from_secs(10), || async {
        let busy: Vec<String> = host
            .workspaces()
            .list()
            .await
            .unwrap()
            .iter()
            .filter_map(|r| r.busy.map(|op| format!("{} {op}", r.id)))
            .collect();
        let maintenance: Vec<String> = rig
            .runtime
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .filter(|name| puddle_workspace::is_maintenance_name(name))
            .collect();
        if busy.is_empty() && maintenance.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "busy: {busy:?}; maintenance sandboxes running: {maintenance:?}"
            ))
        }
    })
    .await;
}

#[derive(Clone, Copy, Debug)]
enum Request {
    Create,
    Start,
    Stop,
    Reclaim,
    ResetRootDisk,
    GrowRootDisk,
    DeleteCheck,
    Delete,
    Clone,
}

impl Request {
    /// The state the workspace has to be in for the request to be accepted.
    fn needs_running(self) -> bool {
        matches!(self, Self::Stop | Self::Clone)
    }
}

/// A workspace in the state `request` needs: `current` when it is still there, else a new one.
async fn ready_for(
    host: &Host<FakeRuntime>,
    rig: &Rig,
    request: Request,
    current: &mut Option<WorkspaceId>,
    made: &mut usize,
) -> WorkspaceId {
    let service = host.workspaces();
    let kept = match current.as_ref() {
        Some(id) => service.get(id).await.ok(),
        None => None,
    };
    let record = if let Some(record) = kept {
        record
    } else {
        *made += 1;
        let name = DisplayName::parse(&format!("dropped-{made}")).unwrap();
        let record = service
            .create(
                NewWorkspace::new(puddle_store::UserId::OWNER, name, Vec::new()),
                &GitThatWaits(host.store()),
            )
            .await
            .unwrap();
        settled(host, rig, "the create ends").await;
        service.get(&record.id).await.unwrap()
    };
    let id = record.id.clone();
    *current = Some(id.clone());
    let running = record.status == WorkspaceStatus::Running;
    if request.needs_running() && !running {
        service.start(&id).await.unwrap();
    } else if !request.needs_running() && running {
        service.stop(&id).await.unwrap();
    }
    settled(host, rig, "the workspace is ready for the request").await;
    id
}

/// Sends `request` for `id` and drops it after `waits` waits; true when it ended first.
async fn send(host: &Host<FakeRuntime>, request: Request, id: &WorkspaceId, waits: usize) -> bool {
    let service = host.workspaces();
    match request {
        Request::Create => {
            let name = DisplayName::parse(&format!("made-{waits}")).unwrap();
            drop_after(
                service.create(
                    NewWorkspace::new(puddle_store::UserId::OWNER, name, Vec::new()),
                    &GitThatWaits(host.store()),
                ),
                waits,
            )
            .await
        }
        Request::Start => drop_after(service.start(id), waits).await,
        Request::Stop => drop_after(service.stop(id), waits).await,
        Request::Reclaim => drop_after(service.reclaim(id), waits).await,
        Request::ResetRootDisk => drop_after(service.reset_root_disk(id), waits).await,
        Request::GrowRootDisk => {
            // Larger every time, so a grow that went through never makes the next one a shrink.
            let size = 20_480 + 1_024 * u32::try_from(waits).unwrap();
            drop_after(service.grow_root_disk(id, size), waits).await
        }
        Request::DeleteCheck => drop_after(service.delete_check(id), waits).await,
        Request::Delete => drop_after(service.delete(id, None), waits).await,
        Request::Clone => {
            let url = RepoUrl::parse(&format!("https://example.org/acme/r{waits}.git")).unwrap();
            let entry = EntryId::new(&format!("a{waits:07x}")).unwrap();
            let new = NewRepository {
                repository: Repository::at_default_directory(entry, url),
                identity: None,
                pull: true,
                push: true,
                local: None,
            };
            drop_after(
                service.add_repository(id, AddRepository::Clone(new), &GitThatWaits(host.store())),
                waits,
            )
            .await
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_dropped_at_any_of_its_awaits_leaves_no_workspace_busy() {
    let rig = Rig::new();
    let host = rig.start().await;
    rig.runtime.wait_in_every_call(true);
    let (mut current, mut made) = (None, 0);
    for request in [
        Request::Create,
        Request::Start,
        Request::Stop,
        Request::Reclaim,
        Request::ResetRootDisk,
        Request::GrowRootDisk,
        Request::DeleteCheck,
        Request::Delete,
        Request::Clone,
    ] {
        let mut waits = 0;
        loop {
            let id = ready_for(&host, &rig, request, &mut current, &mut made).await;
            let ended = send(&host, request, &id, waits).await;
            settled(
                &host,
                &rig,
                &format!("{request:?} dropped after {waits} waits"),
            )
            .await;
            if ended {
                break;
            }
            waits += 1;
            assert!(waits < 500, "{request:?} never ended");
        }
    }
    host.shutdown().await;
}

/// A [`GitSetup`] that makes the workspace list unwritable, then waits once.
struct GitThatBreaksTheList(PathBuf);

impl GitSetup for GitThatBreaksTheList {
    fn set_up<'a>(
        &'a self,
        _workspace: &'a WorkspaceId,
        _repositories: &'a [NewRepository],
    ) -> BoxFuture<'a, ()> {
        make_unwritable(&self.0);
        Box::pin(tokio::task::yield_now())
    }

    fn claim(
        &self,
        _: &WorkspaceId,
        _: puddle_store::UserId,
    ) -> Result<(), puddle_api::WorkspaceError> {
        Ok(())
    }
}

/// A create dropped after its entry was saved takes the entry out of the list Puddle shows even
/// when the list cannot be saved again: the saved entry still says "being created", which the
/// next start drops.
#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_create_whose_list_cannot_be_saved_is_still_gone() {
    let rig = Rig::new();
    let host = rig.start().await;
    let name = DisplayName::parse("unsaved").unwrap();
    let list = list_path(&rig);
    let git = GitThatBreaksTheList(list.clone());
    let ended = {
        let mut create = pin!(host.workspaces().create(
            NewWorkspace::new(puddle_store::UserId::OWNER, name, Vec::new()),
            &git
        ));
        // Dropped at the first wait after the git setup broke the list.
        std::future::poll_fn(|cx| match create.as_mut().poll(cx) {
            Poll::Ready(_) => Poll::Ready(true),
            Poll::Pending if list.is_dir() => Poll::Ready(false),
            Poll::Pending => Poll::Pending,
        })
        .await
    };
    assert!(!ended, "the create waited in its git setup");
    let listed = host.workspaces().list().await.unwrap();
    assert!(listed.is_empty(), "{listed:?}");
}
