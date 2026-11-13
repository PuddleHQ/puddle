// SPDX-License-Identifier: GPL-3.0-or-later
//! Serving the API and the network listener.

use std::path::PathBuf;
use std::time::SystemTime;

use puddle_api::{ApiConfig, ApiServer, ApiToken, RunningApi, Services};
use url::Url;

use crate::{HostConfig, HostError};

/// The API, bound and serving. It registers its own address in the services' endpoint registry
/// (the guard's) and drops that entry when it stops.
pub(super) struct ServedApi {
    pub(super) info: puddle_api::ConnectionInfo,
    pub(super) url: Url,
    pub(super) remote_listener: Option<RemoteListener>,
    pub(super) api: RunningApi,
}

/// The network listener of remote serving, as the operator needs it to set up the proxy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteListener {
    /// The address and port it listens on.
    pub addr: std::net::SocketAddr,
    /// The SHA-256 of its certificate, lower-case hex, for a proxy that pins.
    pub fingerprint: String,
    /// The certificate as PEM (no key), for a proxy that trusts it.
    pub certificate_file: PathBuf,
}

pub(super) async fn serve_api(
    config: &HostConfig,
    services: Services,
) -> Result<ServedApi, HostError> {
    let mut api_config = ApiConfig::with_port(config.api.port);
    api_config
        .extra_origins
        .clone_from(&config.api.extra_origins);
    if let Some(ui) = &config.api.ui {
        api_config.ui = Some(ui.clone());
    }
    api_config.multi_user = config.api.multi_user;
    let mut certificate = None;
    if let Some(remote) = &config.api.remote {
        let mut api_remote = puddle_api::Remote::new(remote.serving.clone());
        if let Some(endpoint) = &remote.network {
            let (network, cert_file) = network_listener(config, &remote.serving, endpoint)?;
            certificate = Some((network.tls().fingerprint(), cert_file));
            api_remote = api_remote.with_network_listener(network);
        }
        api_config.remote = Some(api_remote);
    }
    let server = ApiServer::bind(api_config, ApiToken::generate()?, services).await?;
    let info = server.connection_info();
    if let Some(file) = &config.api.connection_file {
        info.write(file)?;
    }
    let url = Url::parse(&info.url).map_err(|e| HostError::State {
        what: "the API address",
        reason: e.to_string(),
    })?;
    let remote_listener =
        server
            .network_addr()
            .zip(certificate)
            .map(|(addr, (fingerprint, certificate_file))| RemoteListener {
                addr,
                fingerprint,
                certificate_file,
            });
    let api = server.spawn();
    Ok(ServedApi {
        info,
        url,
        remote_listener,
        api,
    })
}

/// The listener's settings with its certificate: made for the interface address and the public
/// host names, kept under the data folder. Returns the listener and the file the proxy's
/// operator copies.
pub(super) fn network_listener(
    config: &HostConfig,
    serving: &puddle_api::RemoteServing,
    endpoint: &puddle_api::NetworkEndpoint,
) -> Result<(puddle_api::NetworkListener, PathBuf), HostError> {
    let mut ips = vec![endpoint.interface()];
    let mut names = Vec::new();
    for host in serving.public_hosts() {
        match host
            .host()
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
        {
            Ok(ip) => ips.push(ip),
            Err(_) => names.push(host.host().to_owned()),
        }
    }
    ips.sort_unstable();
    ips.dedup();
    names.sort_unstable();
    names.dedup();
    let data = config.paths.data();
    let cert = puddle_certs::devcert::listener::ensure(data, &ips, &names, SystemTime::now())
        .map_err(|e| HostError::State {
            what: "the network listener's certificate",
            reason: e.to_string(),
        })?;
    let identity =
        puddle_api::TlsIdentity::new(cert.cert_der().to_vec(), cert.key().pkcs8_der().to_vec());
    let file = data
        .join(puddle_certs::devcert::listener::FOLDER)
        .join(puddle_certs::devcert::listener::CERT_FILE);
    Ok((
        puddle_api::NetworkListener::new(endpoint.clone(), identity),
        file,
    ))
}
