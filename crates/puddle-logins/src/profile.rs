// SPDX-License-Identifier: AGPL-3.0-or-later
//! What Puddle knows about a service's login: where the token comes back, which fields of the
//! answer are tokens, and which hosts the tokens are for.
//!
//! A profile is data, one file per service under `profile/`. Adding a service is a new entry in
//! [`builtin`], not new code: the token
//! endpoints and the hosts that take the tokens, and for each token field what it looks like.
//!
//! The model covers more than the two shipped services need: an endpoint has a method and a path
//! pattern with one wildcard segment, a token field has a (nested) path into the answer, and a slot
//! is a role plus an optional resource, so a service that issues several access tokens at once
//! (one per resource) gets one stand-in each.

use puddle_proxy::TerminationSet;

/// What a captured token is used for. A workspace has at most one stand-in for each role of a
/// profile, so a refresh gives the tool the same stand-in with a new real token behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Role {
    /// The short-lived token the service's API takes.
    Access,
    /// The token that is only ever shown to the token endpoint, to get a new access token.
    Refresh,
    /// A long-lived API key that the login produces next to the tokens.
    ApiKey,
    /// A session token the service mints from the access token (the Copilot extension trades the
    /// GitHub token for one). It is made again at every exchange and its fields carry its expiry,
    /// so every exchange gets a new stand-in and the tool keeps working with the older ones.
    Copilot,
    /// A token that says who signed in (an `OpenID` Connect `id_token`). The tool reads its claims
    /// and no service takes it as a credential, so it is only swapped back toward the token
    /// endpoint.
    IdToken,
}

impl Role {
    /// The role as it appears in names: `access`, `refresh`, `api-key`.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Access => "access",
            Self::Refresh => "refresh",
            Self::ApiKey => "api-key",
            Self::Copilot => "copilot",
            Self::IdToken => "id-token",
        }
    }
}

/// How the stand-in for a token is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StandInKind {
    /// The token's prefix, length and alphabet, random in between; one
    /// stand-in per slot, and a refresh puts a new token behind it.
    Shaped,
    /// A token of `key=value;` fields and a MAC after a colon: the fields are kept, because the
    /// client reads them (expiry, endpoints), and only the MAC is random. The fields are claims,
    /// not the credential.
    Fields,
    /// A JWT keeps its header and payload, because the client reads the claims (who it is for,
    /// when it expires), and gets a random signature of the real length. The claims are not the
    /// credential. A token that is not a JWT is shaped.
    Jwt,
    /// Like [`Self::Shaped`] for a token longer than a service's usual one (a Microsoft refresh
    /// token is over a thousand characters): the same prefix, length and alphabet, and the
    /// registry's full length limit.
    ShapedLong,
}

/// Which answers a field applies to, for a token endpoint that hands out tokens for several
/// resources through one field: Microsoft Entra answers `access_token` for the resource the
/// request named (Azure Resource Manager, Graph, Azure `DevOps`), and each is a different token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Audience {
    /// Every answer.
    Any,
    /// An answer that names one of these resources (case does not matter). The name is looked for
    /// in the answer's `scope` and `resource` fields and in the `aud` claim of the token itself;
    /// the first named field of a path that matches takes the token.
    Named(&'static [&'static str]),
    /// An answer no [`Self::Named`] field of the same path took: the resource is one the profile
    /// does not name, and its tokens share this slot (the slot keeps the last few stand-ins).
    Unnamed,
}

/// What a stand-in is kept under: a role, and for a service that hands out one token per resource
/// (an access token for each of several APIs) the resource's short name. A workspace has at most
/// one stand-in for each slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Slot {
    /// What the token is for.
    pub role: Role,
    /// The resource the token is for, as a short name of lowercase letters, digits and `-`
    /// (at most 12 characters, so it fits in the credential store's entry name), or `None` when
    /// the profile has one token of the role.
    pub resource: Option<&'static str>,
}

impl Slot {
    /// A slot of `role` with no resource: what every shipped profile uses.
    #[must_use]
    pub const fn of(role: Role) -> Self {
        Self {
            role,
            resource: None,
        }
    }

    /// The slot as it appears in names: the role's key, then `-` and the resource when there is one.
    #[must_use]
    pub fn key(self) -> String {
        match self.resource {
            Some(resource) => format!("{}-{resource}", self.role.key()),
            None => self.role.key().to_owned(),
        }
    }
}

impl From<Role> for Slot {
    fn from(role: Role) -> Self {
        Self::of(role)
    }
}

/// The method a token request is sent with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Method {
    /// `POST`: every OAuth token endpoint.
    Post,
    /// `PUT`: a registry's login (`npm login`).
    Put,
    /// `GET`: an exchange that is a read (a session token, a web login's `doneUrl`).
    Get,
    /// Any method.
    Any,
}

impl Method {
    /// Whether a request sent with `method` (as the client wrote it) is one this method covers.
    #[must_use]
    pub fn accepts(self, method: &str) -> bool {
        match self {
            Self::Post => method == "POST",
            Self::Put => method == "PUT",
            Self::Get => method == "GET",
            Self::Any => true,
        }
    }
}

/// Which hosts a stand-in is swapped toward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Scope {
    /// The profile's API hosts and its token endpoints.
    Api,
    /// Only the token endpoints: a refresh token has no business anywhere else.
    TokenEndpoint,
    /// Only these hosts (exact names or `*.suffix`): a token that one API takes, where the profile
    /// has tokens for several. Each is also one of the profile's `api_hosts`, so it is decrypted.
    Hosts(&'static [&'static str]),
}

/// Where the real token behind a stand-in is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Keep {
    /// In the operating system's credential store, read back at the workspace's next start: a
    /// login's tokens.
    Stored,
    /// In memory only, for a token the service mints for one request and that lasts minutes (a
    /// registry's bearer token for one pull). Every answer gets a new stand-in, the last few stay
    /// swapped, nothing is written to the credential store and nothing outlives the workspace's
    /// run: the tool asks for a new one, as it does when one runs out. Such a token is not a login,
    /// so it is not listed among the kept ones.
    Memory,
}

/// One field of a token answer that holds a token.
#[derive(Debug, Clone, Copy)]
pub struct Field {
    /// Where the token is in the answer: a top-level name (`access_token`) or, for a JSON answer, a
    /// dotted path through objects (`data.token`). A form answer has top-level names only.
    pub path: &'static str,
    /// What the token is for.
    pub role: Role,
    /// The resource the token is for, when the service issues one token per resource at once
    /// (see [`Slot::resource`]); `None` otherwise.
    pub resource: Option<&'static str>,
    /// The prefixes the service's tokens of this kind start with (`gho_`); the stand-in keeps the
    /// one the real token has. A token with none of them gets a stand-in with no prefix.
    pub prefixes: &'static [&'static str],
    /// How many of the token's last characters the stand-in keeps. A client may record the end
    /// of a key it was given and compare with it later; no profile needs more than 20.
    pub keep_last: usize,
    /// How the stand-in is made. A [`StandInKind::Fields`] token, and a [`StandInKind::Jwt`] one
    /// that is a JWT, is made again at every exchange (its expiry is in what the client reads),
    /// so the slot's earlier stand-ins stay swapped for a few exchanges.
    pub stand_in: StandInKind,
    /// Whether the field only takes a token that starts with one of `prefixes`. Two fields of one
    /// endpoint may name the same answer field when the answers differ (a token exchange answers
    /// with an API key where the login answered with a JWT): the first field in the list that takes
    /// the token is the one used, so a field with no such condition goes last.
    pub only_prefixed: bool,
    /// Where the stand-in is swapped for the real token.
    pub scope: Scope,
    /// Which answers the field applies to.
    pub audience: Audience,
    /// Where the real token is kept.
    pub keep: Keep,
}

impl Field {
    /// The slot the field's stand-in is kept under.
    #[must_use]
    pub const fn slot(&self) -> Slot {
        Slot {
            role: self.role,
            resource: self.resource,
        }
    }
}

/// A request to one path of one host whose answer holds tokens and whose request may hold the
/// refresh token the tool sends back.
#[derive(Debug, Clone, Copy)]
pub struct Endpoint {
    /// The host, exactly.
    pub host: &'static str,
    /// The method the request is sent with.
    pub method: Method,
    /// The path: exact, or with one wildcard segment. A segment `*` stands for any one segment
    /// (`/{tenant}/oauth2/v2.0/token` is `/*/oauth2/v2.0/token`); a segment that ends in `*`
    /// stands for any segment that starts with the text before it (`org.couchdb.user:*`). The
    /// segments are read as [`path_matches`] says.
    pub path: &'static str,
    /// The top-level request fields that hold one of the workspace's stand-ins (the refresh
    /// token, a token handed in for another), where the stand-in is swapped for the real token.
    /// An endpoint with some is also a token endpoint for [`Scope::TokenEndpoint`].
    pub request_fields: &'static [&'static str],
    /// The answer's token fields.
    pub fields: &'static [Field],
}

/// Whether `path` (in the spelling the matcher reads: lower case, no empty or `.` segments) is the
/// one `pattern` names. A pattern is spelled in the same way and has at most one wildcard segment
/// (see [`Endpoint::path`]); a wildcard never matches an empty segment, and never more than one.
#[must_use]
pub fn path_matches(pattern: &str, path: &str) -> bool {
    let mut wanted = pattern.split('/');
    let mut got = path.split('/');
    loop {
        match (wanted.next(), got.next()) {
            (None, None) => return true,
            (Some(want), Some(have)) => {
                let same = match want.strip_suffix('*') {
                    Some(prefix) => have.len() > prefix.len() && have.starts_with(prefix),
                    None => want == have,
                };
                if !same {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

/// A service whose login Puddle captures.
#[derive(Debug, Clone, Copy)]
pub struct Profile {
    /// A short name for identifiers: `claude`, `github`.
    pub id: &'static str,
    /// What the user calls the service.
    pub name: &'static str,
    /// Where tokens come back.
    pub endpoints: &'static [Endpoint],
    /// The hosts, besides the token endpoints, that take the tokens (exact names or `*.suffix`).
    pub api_hosts: &'static [&'static str],
}

impl Profile {
    /// The name of the stand-in for `role` in logs and the audit: `claude-access`.
    #[must_use]
    pub fn slot_name(&self, role: Role) -> String {
        format!("{}-{}", self.id, role.key())
    }

    /// Every host this profile decrypts, for the workspace's decrypted set: the token endpoints
    /// and the API hosts.
    #[must_use]
    pub fn hosts(&self) -> TerminationSet {
        self.hosts_for(Scope::Api)
    }

    /// The hosts a stand-in with `scope` is swapped toward: for [`Scope::Api`] the API hosts and
    /// every token endpoint's host, for [`Scope::TokenEndpoint`] only the hosts of the endpoints
    /// that take a refresh token, for [`Scope::Hosts`] the hosts it names.
    #[must_use]
    pub fn hosts_for(&self, scope: Scope) -> TerminationSet {
        let mut set = TerminationSet::new();
        let (endpoints, api): (Vec<&Endpoint>, &[&str]) = match scope {
            Scope::Api => (self.endpoints.iter().collect(), self.api_hosts),
            Scope::TokenEndpoint => (
                self.endpoints
                    .iter()
                    .filter(|e| !e.request_fields.is_empty())
                    .collect(),
                &[],
            ),
            Scope::Hosts(hosts) => (Vec::new(), hosts),
        };
        for host in endpoints.iter().map(|e| e.host).chain(api.iter().copied()) {
            // The patterns are literals in this file, and a test checks that each one is accepted.
            let _ = set.insert(host);
        }
        set
    }

    /// Every field of every endpoint, with its endpoint.
    pub fn fields(&self) -> impl Iterator<Item = (&Endpoint, &Field)> + '_ {
        self.endpoints
            .iter()
            .flat_map(|endpoint| endpoint.fields.iter().map(move |field| (endpoint, field)))
    }

    /// The name of the stand-in for `slot` in logs and the audit: `claude-access` for a slot with
    /// no resource, `microsoft-access-arm` for one with.
    #[must_use]
    pub fn slot_name_of(&self, slot: Slot) -> String {
        format!("{}-{}", self.id, slot.key())
    }

    /// The slots whose tokens this profile keeps in the credential store ([`Keep::Stored`]): each
    /// slot with the scope of its stand-in. These are the login.
    #[must_use]
    pub fn slots(&self) -> Vec<(Slot, Scope)> {
        self.slots_kept(Keep::Stored)
    }

    /// The slots whose tokens are held in memory only ([`Keep::Memory`]).
    #[must_use]
    pub fn memory_slots(&self) -> Vec<(Slot, Scope)> {
        self.slots_kept(Keep::Memory)
    }

    fn slots_kept(&self, keep: Keep) -> Vec<(Slot, Scope)> {
        let mut slots: Vec<(Slot, Scope)> = self
            .fields()
            .filter(|(_, field)| field.keep == keep)
            .map(|(_, field)| (field.slot(), field.scope))
            .collect();
        slots.sort_by_key(|(slot, _)| *slot);
        slots.dedup_by_key(|(slot, _)| *slot);
        slots
    }

    /// The roles this profile has stand-ins for, each once (a role with several resources counts
    /// once).
    #[must_use]
    pub fn roles(&self) -> Vec<Role> {
        let mut roles: Vec<Role> = self
            .slots()
            .into_iter()
            .map(|(slot, _)| slot.role)
            .collect();
        roles.dedup();
        roles
    }
}

/// The profiles Puddle ships.
#[must_use]
pub fn builtin() -> &'static [Profile] {
    &[CLAUDE, GITHUB, ENTRA, OPENAI, DOCKER]
}

mod claude;
mod docker;
mod entra;
mod github;
mod openai;

pub use claude::CLAUDE;
pub use docker::DOCKER;
pub use entra::ENTRA;
pub use github::GITHUB;
pub use openai::OPENAI;

#[cfg(test)]
mod tests;
