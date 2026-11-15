// SPDX-License-Identifier: AGPL-3.0-or-later
//! How the host builds its runtime.

use std::future::Future;
use std::path::PathBuf;

use puddle_compute::Runtime;
use puddle_proxy::ProxyUrl;

use crate::HostError;

/// What a [`RuntimeFactory`] opens the runtime with.
#[derive(Debug)]
#[non_exhaustive]
pub struct RuntimeInputs<'a> {
    /// The runtime folder and msb home.
    pub layout: &'a puddle_runtime::RuntimeLayout,
    /// The root every mount source must be inside.
    pub guest_share: PathBuf,
    /// Puddle's data folder: no checkout mount may lie in it or hold it.
    pub data: PathBuf,
    /// The pull proxy, which the runtime's image pulls go through.
    pub pull_proxy: ProxyUrl,
    /// The corporate roots (PEM) the registry client must trust besides the system's.
    pub registry_roots: Vec<String>,
    /// msb's log level for sandbox runtimes.
    pub log_level: Option<String>,
    /// The public login key (an OpenSSH line) that the runtime's SSH server accepts: the only way
    /// to log in through a workspace's endpoint. `None` when the key could not be made; the
    /// server then accepts no login.
    pub ssh_login_key: Option<String>,
}

/// Opens the sandbox runtime. The real one opens msb; tests open a fake.
pub trait RuntimeFactory: Send + Sync {
    /// The runtime type.
    type Runtime: Runtime + Clone;

    /// Opens the runtime.
    ///
    /// # Errors
    ///
    /// [`HostError::Compute`] when it cannot be opened.
    fn open(
        &self,
        inputs: RuntimeInputs<'_>,
    ) -> impl Future<Output = Result<Self::Runtime, HostError>> + Send;
}

/// Opens msb in Puddle's private home.
#[derive(Debug, Default, Clone, Copy)]
pub struct MsbFactory;

impl RuntimeFactory for MsbFactory {
    type Runtime = puddle_compute_msb::MsbRuntime;

    async fn open(&self, inputs: RuntimeInputs<'_>) -> Result<Self::Runtime, HostError> {
        let config = puddle_compute_msb::MsbConfig::new(
            inputs.layout.home(),
            inputs.layout.msb_path(),
            inputs.layout.libkrunfw_path(),
            inputs.guest_share,
        )
        .with_protected_dir(inputs.data)
        .with_registry_proxy(inputs.pull_proxy.expose())
        .with_registry_roots(inputs.registry_roots)
        .with_runtime_log_level(inputs.log_level);
        let config = match inputs.ssh_login_key {
            Some(key) => config.with_ssh_key(key),
            None => config,
        };
        Ok(puddle_compute_msb::MsbRuntime::open(config).await?)
    }
}
