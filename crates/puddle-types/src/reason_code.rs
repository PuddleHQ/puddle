// SPDX-License-Identifier: AGPL-3.0-or-later
//! The names the audit's `reason` field can carry, as types the API schema lists.
//!
//! The record keeps `reason` as one string, and old records and other versions keep reading. That
//! string is one of:
//!
//! - a [`ReasonCode`] (`rule`, `push_denied`, ...);
//! - `<prefix>:<code>` with a [`ReasonPrefix`]: `toggle:` and a [`crate::LocalCategory`] name,
//!   `puddle_request_failed:` and a [`FailedRequestCode`], or `limit:` and a [`LimitCode`],
//!   or `pass_through:` and a [`PassThroughCode`].
//!
//! The enums here are what a reader that explains a reason in words keys its table by, so a code
//! added here without a sentence is a compile error there.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The reason codes that stand alone, without a prefix. Serialises as its [`ReasonCode::as_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[non_exhaustive]
pub enum ReasonCode {
    /// A rule decided.
    Rule,
    /// No rule matched.
    NoRule,
    /// Connection records over the per-workspace limit, summarised.
    Suppressed,
    /// The oldest records written while requests waited, merged into one.
    Merged,
    /// A rule on method and path refused a request.
    RequestRule,
    /// A host with rules on method and path was reached on a port Puddle cannot apply them to.
    RequestRulePort,
    /// The destination is Puddle's own address.
    PuddleEndpoint,
    /// The guest asked for SSH, which the proxy does not carry.
    SshUnsupported,
    /// The guest asked for UDP, which the proxy does not carry.
    UdpUnsupported,
    /// The destination is a local address.
    LocalAddress,
    /// The settings could not be read, so the address guard failed closed.
    SettingsUnreadable,
    /// The rules could not be checked; the proxy failed closed.
    PolicyUnavailable,
    /// Puddle's own request.
    PuddleRequest,
    /// A certificate distribution point named in a root the host synced.
    PkiDistributionPoint,
    /// A client asked for another name than the one it `CONNECT`ed to.
    SniMismatch,
    /// A client refused Puddle's certificate for the host.
    GuestTlsRejected,
    /// Puddle could not make its certificate for the host.
    CertificateUnavailable,
    /// The workspace had its limit of open connections.
    ConnectionLimit,
    /// An IP rule denies the address (only as a `refused_reason`).
    IpRule,
    /// A push the push list does not cover.
    PushDenied,
    /// A fetch the pull list does not cover.
    PullDenied,
    /// A Git address Puddle does not read.
    BadGitPath,
    /// An SSH command that is not `git-upload-pack` or `git-receive-pack`.
    SshCommandUnsupported,
    /// An SSH channel request Puddle does not allow.
    SshChannelRefused,
    /// The Git host did not accept the credential Puddle added.
    CredentialRejected,
    /// The Git credential could not be read.
    CredentialUnavailable,
    /// Two identities cover the repository.
    CredentialAmbiguous,
    /// The workspace's Git settings could not be read.
    GitSettingsUnavailable,
    /// A login Puddle once kept is no longer kept.
    LoginNotKept,
    /// A request carried a token for the wrong endpoint of a sign-in flow.
    TokenEndpointSpliced,
    /// A request body Puddle would not read or buffer.
    PeekRefused,
    /// A request body over the limit Puddle reads.
    BodyTooLarge,
    /// An answer in an encoding Puddle cannot read.
    AnswerEncoding,
    /// An answer that held a real secret value Puddle could not replace.
    AnswerWithheld,
    /// A malformed request.
    BadRequest,
    /// A request for another host than its connection.
    MisdirectedHost,
    /// A request that asked for more than Puddle supports (`Expect`).
    ExpectationUnsupported,
    /// A request head over the limit.
    HeadTooLarge,
    /// A request that did not arrive in time.
    RequestTimeout,
    /// A request in a protocol Puddle does not support.
    ProtocolUnsupported,
    /// An HTTP/2 connection past its limit of open streams.
    StreamLimit,
    /// The host holds no termination for the workspace (it stopped, or a start failed), so the
    /// proxy refuses every connection of it instead of passing them with no rules on method and
    /// path.
    WorkspaceNotRunning,
}

impl ReasonCode {
    /// Every code, in declaration order.
    pub const ALL: [Self; 42] = [
        Self::Rule,
        Self::NoRule,
        Self::Suppressed,
        Self::Merged,
        Self::RequestRule,
        Self::RequestRulePort,
        Self::PuddleEndpoint,
        Self::SshUnsupported,
        Self::UdpUnsupported,
        Self::LocalAddress,
        Self::SettingsUnreadable,
        Self::PolicyUnavailable,
        Self::PuddleRequest,
        Self::PkiDistributionPoint,
        Self::SniMismatch,
        Self::GuestTlsRejected,
        Self::CertificateUnavailable,
        Self::ConnectionLimit,
        Self::IpRule,
        Self::PushDenied,
        Self::PullDenied,
        Self::BadGitPath,
        Self::SshCommandUnsupported,
        Self::SshChannelRefused,
        Self::CredentialRejected,
        Self::CredentialUnavailable,
        Self::CredentialAmbiguous,
        Self::GitSettingsUnavailable,
        Self::LoginNotKept,
        Self::TokenEndpointSpliced,
        Self::PeekRefused,
        Self::BodyTooLarge,
        Self::AnswerEncoding,
        Self::AnswerWithheld,
        Self::BadRequest,
        Self::MisdirectedHost,
        Self::ExpectationUnsupported,
        Self::HeadTooLarge,
        Self::RequestTimeout,
        Self::ProtocolUnsupported,
        Self::StreamLimit,
        Self::WorkspaceNotRunning,
    ];

    /// The name the record and the API use.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rule => "rule",
            Self::NoRule => "no_rule",
            Self::Suppressed => "suppressed",
            Self::Merged => "merged",
            Self::RequestRule => "request_rule",
            Self::RequestRulePort => "request_rule_port",
            Self::PuddleEndpoint => "puddle_endpoint",
            Self::SshUnsupported => "ssh_unsupported",
            Self::UdpUnsupported => "udp_unsupported",
            Self::LocalAddress => "local_address",
            Self::SettingsUnreadable => "settings_unreadable",
            Self::PolicyUnavailable => "policy_unavailable",
            Self::PuddleRequest => "puddle_request",
            Self::PkiDistributionPoint => "pki_distribution_point",
            Self::SniMismatch => "sni_mismatch",
            Self::GuestTlsRejected => "guest_tls_rejected",
            Self::CertificateUnavailable => "certificate_unavailable",
            Self::ConnectionLimit => "connection_limit",
            Self::IpRule => "ip_rule",
            Self::PushDenied => "push_denied",
            Self::PullDenied => "pull_denied",
            Self::BadGitPath => "bad_git_path",
            Self::SshCommandUnsupported => "ssh_command_unsupported",
            Self::SshChannelRefused => "ssh_channel_refused",
            Self::CredentialRejected => "credential_rejected",
            Self::CredentialUnavailable => "credential_unavailable",
            Self::CredentialAmbiguous => "credential_ambiguous",
            Self::GitSettingsUnavailable => "git_settings_unavailable",
            Self::LoginNotKept => "login_not_kept",
            Self::TokenEndpointSpliced => "token_endpoint_spliced",
            Self::PeekRefused => "peek_refused",
            Self::BodyTooLarge => "body_too_large",
            Self::AnswerEncoding => "answer_encoding",
            Self::AnswerWithheld => "answer_withheld",
            Self::BadRequest => "bad_request",
            Self::MisdirectedHost => "misdirected_host",
            Self::ExpectationUnsupported => "expectation_unsupported",
            Self::HeadTooLarge => "head_too_large",
            Self::RequestTimeout => "request_timeout",
            Self::ProtocolUnsupported => "protocol_unsupported",
            Self::StreamLimit => "stream_limit",
            Self::WorkspaceNotRunning => "workspace_not_running",
        }
    }
}

impl fmt::Display for ReasonCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The text before the colon of a reason that carries a code after it. Serialises as its
/// [`ReasonPrefix::as_str`], without the colon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[non_exhaustive]
pub enum ReasonPrefix {
    /// `toggle:<category>`: a switched-off [`crate::LocalCategory`].
    Toggle,
    /// `limit:<code>`: a limit of the proxy ended the connection or cut a stream of it.
    Limit,
    /// `pass_through:<code>`: a host the workspace decrypts was passed through untouched.
    PassThrough,
    /// `puddle_request_failed:<code>`: a [`FailedRequestCode`].
    PuddleRequestFailed,
}

impl ReasonPrefix {
    /// The name the record and the API use, without the colon.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Toggle => "toggle",
            Self::Limit => "limit",
            Self::PassThrough => "pass_through",
            Self::PuddleRequestFailed => "puddle_request_failed",
        }
    }
}

/// Why one of Puddle's own requests (a repository listing, an image pull) did not work: the code
/// after `puddle_request_failed:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[non_exhaustive]
pub enum FailedRequestCode {
    /// The host could not be reached.
    Unreachable,
    /// Its certificate was not trusted.
    Tls,
    /// It did not answer in time.
    Timeout,
    /// The answer was too large.
    TooLarge,
    /// It did not answer like a Git host.
    Protocol,
    /// The token cannot be sent.
    BadToken,
    /// The host answered `401` or `403`.
    HostRefused,
    /// The host answered `429`.
    RateLimited,
    /// The host answered with any other error status.
    HostError,
}

impl FailedRequestCode {
    /// The name the record and the API use.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unreachable => "unreachable",
            Self::Tls => "tls",
            Self::Timeout => "timeout",
            Self::TooLarge => "too_large",
            Self::Protocol => "protocol",
            Self::BadToken => "bad_token",
            Self::HostRefused => "host_refused",
            Self::RateLimited => "rate_limited",
            Self::HostError => "host_error",
        }
    }
}

impl fmt::Display for FailedRequestCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which limit of the proxy ended a connection or cut a stream of it: the code after `limit:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[non_exhaustive]
pub enum LimitCode {
    /// A stream moved no frame for its limit and never gave way to a reset.
    StreamIdle,
    /// The peer did not take the graceful shutdown.
    ShutdownIgnored,
    /// An agreed WebSocket pipe moved no byte for its limit.
    WebsocketIdle,
    /// An HTTP/2 header block stayed open too long.
    HeaderBlockTooSlow,
    /// An HTTP/2 peer sent too many control frames.
    ControlFrameFlood,
}

impl LimitCode {
    /// Every code, in declaration order.
    pub const ALL: [Self; 5] = [
        Self::StreamIdle,
        Self::ShutdownIgnored,
        Self::WebsocketIdle,
        Self::HeaderBlockTooSlow,
        Self::ControlFrameFlood,
    ];

    /// The name the record and the API use.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StreamIdle => "stream_idle",
            Self::ShutdownIgnored => "shutdown_ignored",
            Self::WebsocketIdle => "websocket_idle",
            Self::HeaderBlockTooSlow => "header_block_too_slow",
            Self::ControlFrameFlood => "control_frame_flood",
        }
    }
}

impl fmt::Display for LimitCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a host the workspace decrypts was passed through untouched: the code after `pass_through:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[non_exhaustive]
pub enum PassThroughCode {
    /// The host is on the never-decrypt list.
    NeverDecrypt,
    /// The host is on the curated list of hosts that cannot be decrypted.
    Curated,
    /// A tool did not accept Puddle's certificate for the host before.
    Learned,
    /// The connection came from a container, which does not trust Puddle's certificate.
    Container,
    /// The client did not start a TLS handshake.
    NotTls,
    /// The client asked for another name than the one it `CONNECT`ed to.
    SniMismatch,
    /// The server asked for a client certificate.
    ServerClientCertificate,
    /// The server's certificate was not accepted.
    ServerCertificate,
    /// The TLS handshake with the server failed.
    ServerTls,
}

impl PassThroughCode {
    /// Every code, in declaration order.
    pub const ALL: [Self; 9] = [
        Self::NeverDecrypt,
        Self::Curated,
        Self::Learned,
        Self::Container,
        Self::NotTls,
        Self::SniMismatch,
        Self::ServerClientCertificate,
        Self::ServerCertificate,
        Self::ServerTls,
    ];

    /// The name the record and the API use.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NeverDecrypt => "never_decrypt",
            Self::Curated => "curated",
            Self::Learned => "learned",
            Self::Container => "container",
            Self::NotTls => "not_tls",
            Self::SniMismatch => "sni_mismatch",
            Self::ServerClientCertificate => "server_client_certificate",
            Self::ServerCertificate => "server_certificate",
            Self::ServerTls => "server_tls",
        }
    }
}

impl fmt::Display for PassThroughCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests;
