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

use microsandbox::LocalBackend;
use microsandbox::config::RegistryOptions;
use microsandbox_image::{GlobalCache, Platform, PullOptions, Reference, Registry};
use puddle_compute::{ComputeError, ImageConfig};
use puddle_types::ImageRef;

/// Makes sure `image` is in `local`'s cache and returns its config.
pub(crate) async fn pull(
    local: &LocalBackend,
    roots: &[String],
    image: &ImageRef,
) -> Result<ImageConfig, ComputeError> {
    let fail = |reason: String| ComputeError::ImagePull {
        image: image.to_string(),
        reason,
    };
    let reference: Reference = image
        .as_str()
        .parse()
        .map_err(|e| fail(format!("invalid image reference: {e}")))?;
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
    let result = Box::pin(registry.pull(&reference, &options))
        .await
        .map_err(|e| fail(chain(&e)))?;
    Ok(convert(result.config))
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
