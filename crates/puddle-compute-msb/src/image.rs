// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pulling an image into msb's cache without creating a sandbox, and its config.
//!
//! The SDK 0.7.7 pulls only inside `create`; its image crate does the work, so the adapter calls
//! it the same way `create` does: cache first, then the registry with the backend's registry
//! settings (anonymous unless Puddle's `config.json` says otherwise, which it doesn't), plus
//! Puddle's registry roots ([`crate::MsbConfig::registry_roots`]).
//!
//! The registry client's proxy is the SDK backend's own setting (`LocalBackendBuilder::registry_proxy`,
//! from [`crate::MsbConfig::registry_proxy`]): Puddle's image-pull proxy (`puddle_proxy::PullProxy`).
//! It comes back in the resolved registry settings and replaces the process environment for the
//! client, so nothing sets `HTTPS_PROXY`.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use microsandbox::LocalBackend;
use microsandbox::config::RegistryOptions;
use microsandbox_image::PullProgress as MsbPull;
use microsandbox_image::{
    GlobalCache, ImageError, Platform, PullOptions, PullProgressHandle, Reference, Registry,
    progress_channel,
};
use oci_client::errors::OciDistributionError;
use puddle_compute::{ComputeError, ImageConfig, PullPhase, PullProgress, PullReport};
use puddle_types::ImageRef;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// Makes sure `image` is in `local`'s cache and returns its config.
pub(crate) async fn pull(
    local: &LocalBackend,
    roots: &[String],
    image: &ImageRef,
    report: Option<PullReport<'_>>,
) -> Result<ImageConfig, ComputeError> {
    let fail = |reason: String| ComputeError::ImagePull {
        image: image.to_string(),
        reason,
    };
    let reference: Reference = image
        .as_str()
        .parse()
        .map_err(|e| fail(format!("invalid image reference: {e}")))?;
    // One pull per image at a time: a second pull of the same image would wait on the cache's
    // manifest lock behind the first one's whole download, say nothing, and run into the
    // first-answer deadline without the registry being slow. Behind the first it finds the
    // image in the cache.
    let _turn = turn_for(&reference.to_string()).lock_owned().await;
    let cache = GlobalCache::new(&local.cache_dir()).map_err(|e| fail(chain(&e)))?;
    let options = PullOptions::default();
    if let Some((result, _)) = Registry::pull_cached_async(&cache, &reference, &options)
        .await
        .map_err(|e| fail(chain(&e)))?
    {
        return Ok(convert(result.config));
    }
    let settings = local
        .registry_config(reference.registry(), RegistryOptions::default())
        .await
        .map_err(|e| fail(chain(&e)))?;
    let mut builder = Registry::builder(Platform::host_linux(), cache)
        .auth(settings.auth)
        .extra_ca_certs(with_roots(settings.ca_certs, roots))
        .add_insecure_registries(settings.insecure_registries);
    if let Some(proxy) = settings.proxy {
        builder = builder.proxy(proxy);
    }
    let registry = builder.build().map_err(|e| fail(chain(&e)))?;
    // The pull runs on its own task so this one can read its progress; dropping this future (a
    // cancelled create) ends the task, as it ended the pull before.
    let (mut events, sender) = progress_channel();
    let mut task = EndsOnDrop(registry.pull_with_sender(&reference, &options, sender));
    let mut tally = Tally::default();
    let report = report.unwrap_or(&|_| {});
    if !drain_progress(&mut events, FIRST_ANSWER_DEADLINE, &mut tally, report).await {
        return Err(fail(no_answer(reference.registry(), FIRST_ANSWER_DEADLINE)));
    }
    let result = (&mut task.0)
        .await
        .map_err(|e| fail(format!("the download task ended: {e}")))?
        .map_err(|e| fail(explain_connect(&e, reference.registry())))?;
    Ok(convert(result.config))
}

/// The turn for pulling `image` (a reference string): the same lock for the same image.
fn turn_for(image: &str) -> Arc<AsyncMutex<()>> {
    static TURNS: LazyLock<Mutex<BTreeMap<String, Arc<AsyncMutex<()>>>>> =
        LazyLock::new(Mutex::default);
    let mut turns = TURNS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(turns.entry(image.to_owned()).or_default())
}

/// How long the registry has to answer the manifest request before the pull gives up. The pull
/// proxy's connect timeout (10 s) runs once per try of the registry client, which tries five times
/// with 15 s of pauses between: a route that swallows every packet took 76 s to fail. A registry
/// that answers at all does so well inside this, even over a lossy 300 ms link; once the manifest
/// is read the pull has no such limit.
const FIRST_ANSWER_DEADLINE: Duration = Duration::from_secs(20);

/// Feeds `events` to `tally` and `report` until the pull ends. Returns `false` when the manifest
/// has not been read within `first_answer` of the start, `true` otherwise.
async fn drain_progress(
    events: &mut PullProgressHandle,
    first_answer: Duration,
    tally: &mut Tally,
    report: PullReport<'_>,
) -> bool {
    let deadline = Instant::now() + first_answer;
    loop {
        let event = if tally.layers > 0 {
            events.recv().await
        } else {
            match tokio::time::timeout_at(deadline, events.recv()).await {
                Ok(event) => event,
                Err(_) => return false,
            }
        };
        let Some(event) = event else { return true };
        if let Some(progress) = tally.apply(&event) {
            report(progress);
        }
    }
}

/// The reason a pull gives up before the registry answered.
fn no_answer(registry: &str, waited: Duration) -> String {
    format!(
        "could not connect to {registry}: no answer in {} s (the registry client was still retrying)",
        waited.as_secs()
    )
}

/// A task that is aborted when its handle is dropped before it finished.
struct EndsOnDrop<T>(JoinHandle<T>);

impl<T> Drop for EndsOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Adds the per-layer events of msb's pull up to the whole image's counts.
#[derive(Default)]
struct Tally {
    layers: usize,
    download_total: Option<u64>,
    downloaded: BTreeMap<usize, u64>,
    download_sizes: BTreeMap<usize, u64>,
    unpacked: BTreeMap<usize, u64>,
    unpack_sizes: BTreeMap<usize, u64>,
}

impl Tally {
    /// The progress `event` makes, or `None` when it changes no count.
    fn apply(&mut self, event: &MsbPull) -> Option<PullProgress> {
        match event {
            MsbPull::Resolved {
                layer_count,
                total_download_bytes,
                ..
            } => {
                self.layers = *layer_count;
                self.download_total = *total_download_bytes;
                Some(self.downloading())
            }
            MsbPull::LayerDownloadProgress {
                layer_index,
                downloaded_bytes,
                total_bytes,
                ..
            } => {
                self.downloaded.insert(*layer_index, *downloaded_bytes);
                if let Some(total) = total_bytes {
                    self.download_sizes.insert(*layer_index, *total);
                }
                Some(self.downloading())
            }
            MsbPull::LayerDownloadComplete {
                layer_index,
                downloaded_bytes,
                ..
            } => {
                self.downloaded.insert(*layer_index, *downloaded_bytes);
                self.download_sizes.insert(*layer_index, *downloaded_bytes);
                Some(self.downloading())
            }
            MsbPull::LayerMaterializeProgress {
                layer_index,
                bytes_read,
                total_bytes,
            } => {
                self.unpacked.insert(*layer_index, *bytes_read);
                self.unpack_sizes.insert(*layer_index, *total_bytes);
                // Layers unpack one after another, so a total that grows with each layer would
                // make the share fall back: it is given only once every layer has said its size.
                let known = self.layers > 0 && self.unpack_sizes.len() >= self.layers;
                Some(PullProgress {
                    phase: PullPhase::Unpacking,
                    done: self.unpacked.values().sum(),
                    total: known.then(|| self.unpack_sizes.values().sum()),
                })
            }
            _ => None,
        }
    }

    fn downloading(&self) -> PullProgress {
        let from_layers = (self.layers > 0 && self.download_sizes.len() >= self.layers)
            .then(|| self.download_sizes.values().sum());
        PullProgress {
            phase: PullPhase::Downloading,
            done: self.downloaded.values().sum(),
            total: self.download_total.or(from_layers),
        }
    }
}

/// `err` and its causes, `: `-separated. The registry client's top-level error is only "error
/// sending request"; the cause says why (an unknown certificate issuer behind an intercepting
/// proxy, a refused proxy, a DNS failure), which is what the user needs to see.
fn chain(err: &dyn std::error::Error) -> String {
    let mut text = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.ends_with(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
}

/// `err`'s text, led by a line that says Puddle could not connect to `registry` when the client
/// reports that the connection was never made (a refused or swallowed route, a proxy that refused
/// the tunnel); any other error keeps its own text. The pull proxy's 502 text ("could not connect
/// to host:port") cannot reach this far: the client's tunnel reads only the status of the proxy's
/// answer and drops its reason phrase and body (hyper-util's `TunnelUnsuccessful`), so this lead
/// is the only place the user's text can say it.
fn explain_connect(err: &ImageError, registry: &str) -> String {
    let reason = chain(err);
    match err {
        ImageError::Registry(OciDistributionError::RequestError(e)) if e.is_connect() => {
            format!("could not connect to {registry}: {reason}")
        }
        _ => reason,
    }
}

/// msb's configured CA certificates (PEM each) followed by Puddle's registry roots.
fn with_roots(mut configured: Vec<Vec<u8>>, roots: &[String]) -> Vec<Vec<u8>> {
    configured.extend(roots.iter().map(|pem| pem.as_bytes().to_vec()));
    configured
}

/// The OCI config as Puddle's [`ImageConfig`]. `ENV` entries without `=` are kept with an empty
/// value; labels are sorted by key.
pub(crate) fn convert(config: microsandbox_image::ImageConfig) -> ImageConfig {
    ImageConfig {
        entrypoint: config.entrypoint.unwrap_or_default(),
        cmd: config.cmd.unwrap_or_default(),
        env: config
            .env
            .into_iter()
            .map(|entry| match entry.split_once('=') {
                Some((k, v)) => (k.to_owned(), v.to_owned()),
                None => (entry, String::new()),
            })
            .collect(),
        working_dir: config.working_dir.filter(|w| !w.is_empty()),
        user: config.user.filter(|u| !u.is_empty()),
        labels: config.labels.into_iter().collect::<BTreeMap<_, _>>(),
    }
}

#[cfg(test)]
mod tests;
