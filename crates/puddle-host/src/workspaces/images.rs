// SPDX-License-Identifier: AGPL-3.0-or-later
//! Image pulls and the warm-ups of the default and the maintenance image.

use std::future::Future;
use std::sync::{Mutex, PoisonError};

use puddle_compute::{ImageConfig, PullPhase, PullProgress, Runtime};
use puddle_services::{workspaces::WorkspaceError, workspaces::WorkspaceRecord};
use puddle_types::{Event, EventSink, ImageRef, WorkspaceId, WorkspaceStep};

use super::{HostWorkspaces, ImageNote, MAINTENANCE_WAIT, Warmth, wait_until_downloaded};

use crate::op_error::OpError;

impl<R: Runtime + Clone> HostWorkspaces<R> {
    /// A pull's count as the step it belongs to (downloading or unpacking), with the bytes done
    /// and, when known, the bytes in all.
    fn progress_pull(&self, name: &WorkspaceId, pull: PullProgress) {
        let step = match pull.phase {
            PullPhase::Downloading => WorkspaceStep::PullingImage,
            PullPhase::Unpacking => WorkspaceStep::UnpackingImage,
        };
        self.inner.events.emit(Event::WorkspaceProgress {
            workspace: name.clone(),
            step,
            detail: None,
            repository: None,
            bytes_done: Some(pull.done),
            bytes_total: pull.total,
        });
    }

    /// The workspace's image and what the runtime says about it, pulled if it is not local yet.
    pub(super) async fn pulled_image(
        &self,
        record: &WorkspaceRecord,
        announce_pull: bool,
    ) -> Result<(ImageRef, ImageConfig), OpError> {
        let image = ImageRef::new(&record.image)?;
        if announce_pull {
            self.progress(
                &record.id,
                WorkspaceStep::PullingImage,
                Some(record.image.clone()),
            );
        }
        let config = if announce_pull {
            let throttle = PullThrottle::default();
            let report = |pull: PullProgress| {
                if throttle.lets_through(pull) {
                    self.progress_pull(&record.id, pull);
                }
            };
            self.inner
                .runtime
                .pull_image_reporting(&image, &report)
                .await?
        } else {
            self.image_config(&image).await?
        };
        Ok((image, config))
    }

    /// The image config of `image`, pulling it if needed.
    pub(super) async fn image_config(&self, image: &ImageRef) -> Result<ImageConfig, OpError> {
        Ok(self.inner.runtime.pull_image(image).await?)
    }

    /// The line the default image's labels give, or `None` while they are not read. The first
    /// call starts a task that downloads the image (the create needs it anyway) and keeps the
    /// answer; a failed download is tried again by the next call. Never waits for the download.
    pub(crate) fn default_image_note(&self) -> Option<String> {
        {
            let mut note = self
                .inner
                .default_image_note
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            match &*note {
                ImageNote::Read(line) => return line.clone(),
                ImageNote::Reading => return None,
                ImageNote::Cold => *note = ImageNote::Reading,
            }
        }
        let this = self.clone();
        self.spawn(async move {
            let next = match ImageRef::new(puddle_services::workspaces::DEFAULT_IMAGE) {
                Ok(image) => match this.inner.runtime.pull_image(&image).await {
                    Ok(config) => ImageNote::Read(puddle_services::workspaces::describe_image(&config.labels)),
                    Err(e) => {
                        tracing::warn!(image = %image, error = %e, "the default image was not downloaded to read its description");
                        ImageNote::Cold
                    }
                },
                Err(_) => ImageNote::Read(None),
            };
            *this
                .inner
                .default_image_note
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = next;
        });
        None
    }

    /// Starts pulling the image the delete check boots on a task the service owns, unless it is
    /// cached or a pull runs. The check can't go ahead without it, and the first pull of an image
    /// that isn't the workspace's own takes minutes, longer than a request may.
    pub(crate) fn warm_maintenance_image(&self) {
        let start = self.inner.maintenance_image.send_if_modified(|w| {
            let idle = matches!(w, Warmth::Cold | Warmth::Failed);
            if idle {
                *w = Warmth::Warming;
            }
            idle
        });
        if !start {
            return;
        }
        let this = self.clone();
        self.spawn(async move {
            let image = this.inner.workspaces.config().maintenance_image.clone();
            let next = match this.inner.runtime.pull_image(&image).await {
                Ok(_) => Warmth::Ready,
                Err(e) => {
                    tracing::warn!(image = %image, error = %e, "the image the delete check boots was not downloaded");
                    Warmth::Failed
                }
            };
            this.inner.maintenance_image.send_replace(next);
        });
    }

    /// [`HostWorkspaces::warm_maintenance_image`] when the service starts with workspaces
    /// listed: one of them may be deleted at once.
    pub(crate) fn warm_maintenance_image_for_listed(&self) {
        let listed = !self.state().slots.is_empty();
        if listed {
            self.warm_maintenance_image();
        }
    }

    /// Waits (up to [`MAINTENANCE_WAIT`]) for the image the delete check boots to be in the
    /// cache, starting the download when nothing has. A request that cannot wait that long gets
    /// an answer that says what is going on; the download goes on without it.
    pub(super) async fn maintenance_ready(&self) -> Result<(), WorkspaceError> {
        self.warm_maintenance_image();
        wait_until_downloaded(
            &mut self.inner.maintenance_image.subscribe(),
            MAINTENANCE_WAIT,
        )
        .await
    }

    /// Runs a job that boots the maintenance image: waits for the image first (see
    /// [`HostWorkspaces::maintenance_ready`]), then runs the job on a task the service owns (see
    /// [`HostWorkspaces::on_own_task`]). Every maintenance job a request waits for passes here.
    pub(super) async fn maintenance_job<T: Send + 'static>(
        &self,
        job: impl Future<Output = T> + Send + 'static,
    ) -> Result<T, WorkspaceError> {
        self.maintenance_ready().await?;
        self.on_own_task(job).await
    }
}

/// Keeps a pull's reports to the ones a person can see change: a new phase, a whole percent more
/// when the total is known, and every 8 MiB when it is not. msb reports per chunk, many times a
/// second.
#[derive(Default)]
pub(super) struct PullThrottle(Mutex<Option<(PullPhase, u64)>>);

impl PullThrottle {
    pub(super) fn lets_through(&self, pull: PullProgress) -> bool {
        const UNKNOWN_TOTAL_STEP: u64 = 8 * 1024 * 1024;
        let at = match pull.total {
            Some(total) if total > 0 => pull.done.min(total).saturating_mul(100) / total,
            _ => pull.done / UNKNOWN_TOTAL_STEP,
        };
        let key = (pull.phase, at);
        let mut last = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let new = *last != Some(key);
        *last = Some(key);
        new
    }
}
