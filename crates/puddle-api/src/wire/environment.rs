// SPDX-License-Identifier: AGPL-3.0-or-later
//! Environment variables and secrets on the wire (credentials spec §9). A plain variable's value
//! is shown and changed here. A secret's value is write-only: a request carries it once, to be
//! kept in the operating system's credential store, and no response, error or log line carries it,
//! its length or any part of it.

use puddle_secrets::Secret;
use puddle_store as store;
use puddle_types::WorkspaceId;
use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Where a variable applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EnvScope {
    /// Every workspace gets it, unless the workspace has its own of the same name.
    Global,
    /// Only this workspace gets it.
    Workspace,
}

/// What a variable is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EnvKind {
    /// The workspace gets the value as it is.
    Plain,
    /// The workspace gets a stand-in; Puddle adds the real value on the way out to the secret's
    /// hosts only.
    Secret,
}

/// One variable. A secret's value is never part of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EnvVariable {
    /// The variable's name.
    pub name: String,
    /// Where it applies.
    pub scope: EnvScope,
    /// What it is.
    pub kind: EnvKind,
    /// A plain variable's value; `null` for a secret.
    #[schema(required = true)]
    pub value: Option<String>,
    /// The hosts a secret's real value may be sent to (names, or `*.` and a name for every name
    /// below it); empty for a plain variable.
    pub hosts: Vec<String>,
    /// Where in a request a secret's stand-in is swapped; `null` for a plain variable.
    #[schema(required = true)]
    pub place: Option<SecretPlace>,
    /// A global variable that the workspace's own variable of the same name hides. Only ever
    /// `true` in a workspace's list.
    pub overridden: bool,
    /// When it last changed, epoch ms.
    pub changed_at: u64,
}

impl EnvVariable {
    pub(crate) fn from_store(entry: store::EnvEntry, overridden: bool) -> Self {
        let (kind, value, hosts, place) = match entry.value {
            store::EnvValue::Plain(value) => (EnvKind::Plain, Some(value), Vec::new(), None),
            store::EnvValue::Secret(secret) => (
                EnvKind::Secret,
                None,
                secret.hosts.iter().map(ToString::to_string).collect(),
                Some(SecretPlace::from_store(secret.place)),
            ),
        };
        Self {
            name: entry.name.to_string(),
            scope: match entry.scope {
                store::EnvScope::Global => EnvScope::Global,
                store::EnvScope::Workspace(_) => EnvScope::Workspace,
            },
            kind,
            value,
            hosts,
            place,
            overridden,
            changed_at: entry.changed_at,
        }
    }
}

/// Where in a request Puddle swaps a secret's stand-in for its real value, toward the secret's
/// hosts only. Anywhere else the stand-in goes out unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "in", rename_all = "snake_case", deny_unknown_fields)]
pub enum SecretPlace {
    /// The values of request headers: what almost every tool uses.
    Header {},
    /// The one query parameter `name` (`appid`, `access_token`, `key`), for an API that takes its
    /// key nowhere else.
    Query {
        /// The parameter's name: 1 to 64 visible characters, none of `&`, `=`, `#`, `%`, `+`.
        name: String,
    },
    /// The path (a Telegram bot's `/bot<token>/`, a webhook URL).
    Path {},
}

impl SecretPlace {
    fn from_store(place: store::SecretPlace) -> Self {
        match place {
            store::SecretPlace::Header => Self::Header {},
            store::SecretPlace::Query(name) => Self::Query { name: name.into() },
            store::SecretPlace::Path => Self::Path {},
        }
    }

    /// The place to store; none given keeps `stored` (header values for a new secret).
    ///
    /// # Errors
    /// [`store::StoreError::EnvInvalid`] for a query name that is not one.
    pub(crate) fn into_store(
        place: Option<Self>,
        stored: Option<store::SecretPlace>,
    ) -> Result<store::SecretPlace, store::StoreError> {
        match place {
            None => Ok(stored.unwrap_or_default()),
            Some(Self::Header {}) => Ok(store::SecretPlace::Header),
            Some(Self::Query { name }) => puddle_types::QueryName::new(&name)
                .map(store::SecretPlace::Query)
                .map_err(|e| store::StoreError::EnvInvalid(e.to_string())),
            Some(Self::Path {}) => Ok(store::SecretPlace::Path),
        }
    }
}

/// The global variables, by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EnvList {
    /// The variables.
    pub variables: Vec<EnvVariable>,
}

/// What a workspace sees: its own variables and the global ones, by name. A global variable that
/// the workspace's own of the same name hides is listed after it with `overridden` set. The
/// workspace's environment is read when it starts: a change reaches a running workspace's own
/// environment at its next start; a secret's hosts and value apply at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WorkspaceEnv {
    /// The workspace.
    pub workspace: WorkspaceId,
    /// Its variables.
    pub variables: Vec<EnvVariable>,
}

/// A secret's value in a request: written once, kept in the credential store, never shown again.
/// Only a JSON string is accepted, and the refusal of anything else does not quote it.
pub struct SecretText(pub(crate) Secret);

impl std::fmt::Debug for SecretText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

impl<'de> Deserialize<'de> for SecretText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Text;

        impl<'de> Visitor<'de> for Text {
            type Value = SecretText;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a string")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<SecretText, E> {
                Ok(SecretText(Secret::new(value.to_owned())))
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<SecretText, E> {
                Ok(SecretText(Secret::new(value)))
            }

            // Whatever else arrives, the message names the kind and not the content: a secret
            // sent as a number must not come back in the refusal.
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<SecretText, E> {
                Err(E::custom("the value must be a string, not a boolean"))
            }

            fn visit_i64<E: de::Error>(self, _: i64) -> Result<SecretText, E> {
                Err(E::custom("the value must be a string, not a number"))
            }

            fn visit_u64<E: de::Error>(self, _: u64) -> Result<SecretText, E> {
                Err(E::custom("the value must be a string, not a number"))
            }

            fn visit_f64<E: de::Error>(self, _: f64) -> Result<SecretText, E> {
                Err(E::custom("the value must be a string, not a number"))
            }

            fn visit_seq<A: de::SeqAccess<'de>>(self, _: A) -> Result<SecretText, A::Error> {
                Err(de::Error::custom("the value must be a string, not a list"))
            }

            fn visit_map<A: de::MapAccess<'de>>(self, _: A) -> Result<SecretText, A::Error> {
                Err(de::Error::custom(
                    "the value must be a string, not an object",
                ))
            }
        }

        deserializer.deserialize_any(Text)
    }
}

/// What to set a variable to.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EnvSetRequest {
    /// A plain variable.
    Plain {
        /// The value.
        value: String,
    },
    /// A secret. A new one needs its `value`; one that is already a secret keeps its value when
    /// `value` is left out, so its hosts can change without typing it again.
    Secret {
        /// The value: written once, kept in the operating system's credential store, never shown
        /// again.
        #[serde(default)]
        #[schema(value_type = Option<String>, format = Password, write_only, required = false)]
        value: Option<SecretText>,
        /// The hosts the real value may be sent to, at least one: names, or `*.` and a name for
        /// every name below it. A pattern over a domain anyone can register a name under
        /// (`*.github.io`) is refused.
        hosts: Vec<String>,
        /// Where in a request the stand-in is swapped. Left out: the secret's stored place when it
        /// is a secret already, header values for a new one.
        #[serde(default)]
        place: Option<SecretPlace>,
    },
}

#[cfg(test)]
mod tests;
