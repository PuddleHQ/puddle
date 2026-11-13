// SPDX-License-Identifier: AGPL-3.0-or-later
//! The body of a token request or answer: a JSON object or a URL-encoded form, with typed access
//! to its top-level text fields.
//!
//! A login's token endpoint is the one place Puddle reads a body: Claude Code posts and answers
//! JSON, `gh` posts and reads a form, and GitHub answers either, by the request's `Accept`. The
//! format follows the message's own `Content-Type`, in both directions, so no profile says
//! which one a service uses. Only top-level text fields are read or replaced; every other part of
//! the body goes through as it was (a form keeps the exact bytes of the fields that were not
//! touched).

use std::borrow::Cow;

use serde_json::{Map, Value};
use zeroize::{Zeroize as _, Zeroizing};

use super::wiped::Wiped;

/// How the body is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyFormat {
    /// A JSON object (`application/json`, or a `+json` type).
    Json,
    /// `application/x-www-form-urlencoded`.
    Form,
}

/// What a field of a body holds, without the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// A text value (a JSON string, a form value) of this many characters.
    Text(usize),
    /// A JSON `null` or an empty text.
    Empty,
    /// A JSON object or array (its own fields are listed after it, one level deeper).
    Structured,
    /// A number or a boolean.
    Other,
}

/// One field of a body, as [`TokenBody::fields`] lists it: a name and a kind, never the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyField {
    /// The field's name; an element of an array has its array's name.
    pub name: String,
    /// The dotted path from the top of the body (`data.token`), the spelling a profile's field
    /// path uses; an element of an array has its array's path, a form field its name.
    pub path: String,
    /// What the field holds.
    pub kind: FieldKind,
}

/// Why a body could not be read as a token message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TokenBodyError {
    /// The content type is neither JSON nor a form.
    #[error("the body is neither JSON nor a form")]
    Format,
    /// The body is not valid text for its content type, or not a JSON object.
    #[error("the body does not parse")]
    Malformed,
}

/// One top-level field of a form: its text as sent, and what it says once decoded.
#[derive(Debug, Clone)]
struct Pair {
    raw: String,
    key: String,
    value: String,
}

#[derive(Debug, Clone)]
enum Parsed {
    Json(Map<String, Value>),
    Form(Vec<Pair>),
}

/// A token request or answer body, parsed by its content type.
///
/// ```
/// use puddle_proxy::terminate::TokenBody;
///
/// let mut body = TokenBody::parse(Some("application/json"), br#"{"access_token":"a1","expires_in":60}"#).unwrap();
/// assert_eq!(body.text("access_token").as_deref(), Some("a1"));
/// body.set_text("access_token", "b2");
/// assert_eq!(*body.render(), br#"{"access_token":"b2","expires_in":60}"#);
/// ```
#[derive(Debug, Clone)]
pub struct TokenBody {
    parsed: Parsed,
}

/// The media type of a `Content-Type` value, lower-cased and without parameters.
fn media_type(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

impl TokenBody {
    /// Whether [`Self::parse`] reads bodies of this `content_type`: JSON or a form.
    #[must_use]
    pub fn reads(content_type: Option<&str>) -> bool {
        let media = media_type(content_type.unwrap_or_default());
        media == "application/json"
            || media.ends_with("+json")
            || media == "application/x-www-form-urlencoded"
    }

    /// Parses `body` as the format `content_type` names.
    ///
    /// # Errors
    /// [`TokenBodyError::Format`] for another content type (or none), [`TokenBodyError::Malformed`]
    /// for a body that is not valid in its format.
    pub fn parse(content_type: Option<&str>, body: &[u8]) -> Result<Self, TokenBodyError> {
        let media = media_type(content_type.unwrap_or_default());
        if media == "application/json" || media.ends_with("+json") {
            let Value::Object(map) =
                serde_json::from_slice(body).map_err(|_| TokenBodyError::Malformed)?
            else {
                return Err(TokenBodyError::Malformed);
            };
            return Ok(Self {
                parsed: Parsed::Json(map),
            });
        }
        if media == "application/x-www-form-urlencoded" {
            let text = std::str::from_utf8(body).map_err(|_| TokenBodyError::Malformed)?;
            let pairs = text
                .split('&')
                .filter(|raw| !raw.is_empty())
                .map(|raw| {
                    let (key, value) = url::form_urlencoded::parse(raw.as_bytes())
                        .next()
                        .unwrap_or((Cow::Borrowed(""), Cow::Borrowed("")));
                    Pair {
                        raw: raw.to_owned(),
                        key: key.into_owned(),
                        value: value.into_owned(),
                    }
                })
                .collect();
            return Ok(Self {
                parsed: Parsed::Form(pairs),
            });
        }
        Err(TokenBodyError::Format)
    }

    /// The format the body is in.
    #[must_use]
    pub fn format(&self) -> BodyFormat {
        match self.parsed {
            Parsed::Json(_) => BodyFormat::Json,
            Parsed::Form(_) => BodyFormat::Form,
        }
    }

    /// The text of the top-level field `name`: a JSON string, or a form value decoded. `None`
    /// when there is no such field, it is not text, or (a form) it appears more than once, so
    /// that what Puddle reads is what a server picking the first or the last one would read.
    #[must_use]
    pub fn text(&self, name: &str) -> Option<Cow<'_, str>> {
        match &self.parsed {
            Parsed::Json(map) => map.get(name)?.as_str().map(Cow::Borrowed),
            Parsed::Form(pairs) => {
                let mut found = pairs.iter().filter(|pair| pair.key == name);
                let first = found.next()?;
                found.next().is_none().then(|| Cow::Borrowed(&*first.value))
            }
        }
    }

    /// Every field of the body, nested ones included, in order and with repeats: what a reader
    /// that took the first, the last or the deepest one would find. No value is returned.
    #[must_use]
    pub fn fields(&self) -> Vec<BodyField> {
        fn kind_of(value: &Value) -> FieldKind {
            match value {
                Value::Null => FieldKind::Empty,
                Value::String(text) if text.is_empty() => FieldKind::Empty,
                Value::String(text) => FieldKind::Text(text.chars().count()),
                Value::Object(_) | Value::Array(_) => FieldKind::Structured,
                Value::Bool(_) | Value::Number(_) => FieldKind::Other,
            }
        }
        fn walk(name: &str, path: &str, value: &Value, out: &mut Vec<BodyField>) {
            out.push(BodyField {
                name: name.to_owned(),
                path: path.to_owned(),
                kind: kind_of(value),
            });
            match value {
                Value::Object(map) => {
                    for (key, inner) in map {
                        walk(key, &format!("{path}.{key}"), inner, out);
                    }
                }
                Value::Array(items) => {
                    for inner in items {
                        walk(name, path, inner, out);
                    }
                }
                Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
            }
        }
        let mut out = Vec::new();
        match &self.parsed {
            Parsed::Json(map) => {
                for (key, value) in map {
                    walk(key, key, value, &mut out);
                }
            }
            Parsed::Form(pairs) => {
                for pair in pairs {
                    let kind = if pair.value.is_empty() {
                        FieldKind::Empty
                    } else {
                        FieldKind::Text(pair.value.chars().count())
                    };
                    out.push(BodyField {
                        name: pair.key.clone(),
                        path: pair.key.clone(),
                        kind,
                    });
                }
            }
        }
        out
    }

    /// Replaces the text of the field `name` with `value`; `false` (nothing changed) when
    /// [`Self::text`] would not have found it.
    pub fn set_text(&mut self, name: &str, value: &str) -> bool {
        // `text` is the one rule for which field counts: a field it would not find is not changed.
        if self.text(name).is_none() {
            return false;
        }
        match &mut self.parsed {
            Parsed::Json(map) => map
                .get_mut(name)
                .map(|slot| *slot = Value::String(value.to_owned()))
                .is_some(),
            Parsed::Form(pairs) => pairs
                .iter_mut()
                .find(|pair| pair.key == name)
                .map(|pair| {
                    let encode = |text: &str| -> String {
                        url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
                    };
                    pair.raw = format!("{}={}", encode(&pair.key), encode(value));
                    value.clone_into(&mut pair.value);
                })
                .is_some(),
        }
    }

    /// The text at `path`: a dotted path through the objects of a JSON body (`data.token`), or, for
    /// a form, the field named `path` (a form has no nesting). A path of one segment is
    /// [`Self::text`]. `None` when any step is missing or the end is not a string.
    #[must_use]
    pub fn text_at(&self, path: &str) -> Option<Cow<'_, str>> {
        match &self.parsed {
            Parsed::Json(map) => {
                let (last, parents) = split_path(path)?;
                let mut object = map;
                for step in parents {
                    object = object.get(step)?.as_object()?;
                }
                object.get(last)?.as_str().map(Cow::Borrowed)
            }
            Parsed::Form(_) => self.text(path),
        }
    }

    /// Replaces the text at `path` (see [`Self::text_at`]) with `value`; `false` (nothing changed)
    /// when [`Self::text_at`] would not have found it.
    pub fn set_text_at(&mut self, path: &str, value: &str) -> bool {
        match &mut self.parsed {
            Parsed::Json(map) => {
                let Some(slot) = split_path(path).and_then(|(last, parents)| {
                    let mut object = map;
                    for step in parents {
                        object = object.get_mut(step)?.as_object_mut()?;
                    }
                    object.get_mut(last)
                }) else {
                    return false;
                };
                if !slot.is_string() {
                    return false;
                }
                *slot = Value::String(value.to_owned());
                true
            }
            Parsed::Form(_) => self.set_text(path, value),
        }
    }

    /// The body as bytes: JSON compact, a form with each untouched field as it was sent. The
    /// bytes are zeroised when dropped, since they may hold a token.
    #[must_use]
    pub fn render(&self) -> Zeroizing<Vec<u8>> {
        self.render_wiped().into_zeroizing()
    }

    /// [`render`](Self::render) as a shared buffer for a request body: overwritten when the last
    /// clone drops, and never copied through a buffer that is freed as it is.
    pub(crate) fn render_bytes(&self) -> bytes::Bytes {
        self.render_wiped().into_bytes()
    }

    fn render_wiped(&self) -> Wiped {
        let mut out = Wiped::new();
        match &self.parsed {
            Parsed::Json(map) => {
                // Writing a JSON value into a growing buffer does not fail.
                let _ = serde_json::to_writer(&mut out, map);
            }
            Parsed::Form(pairs) => {
                for (index, pair) in pairs.iter().enumerate() {
                    if index > 0 {
                        out.extend_from_slice(b"&");
                    }
                    out.extend_from_slice(pair.raw.as_bytes());
                }
            }
        }
        out
    }
}

/// The last step of a dotted path and the steps before it; `None` for an empty step.
fn split_path(path: &str) -> Option<(&str, Vec<&str>)> {
    let mut steps: Vec<&str> = path.split('.').collect();
    if steps.iter().any(|step| step.is_empty()) {
        return None;
    }
    let last = steps.pop()?;
    Some((last, steps))
}

/// Wipes every text in `value`: a body can hold a token.
fn wipe(value: &mut Value) {
    match value {
        Value::String(text) => text.zeroize(),
        Value::Array(items) => items.iter_mut().for_each(wipe),
        Value::Object(map) => map.values_mut().for_each(wipe),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

impl Drop for TokenBody {
    fn drop(&mut self) {
        match &mut self.parsed {
            Parsed::Json(map) => map.values_mut().for_each(wipe),
            Parsed::Form(pairs) => {
                for pair in pairs {
                    pair.raw.zeroize();
                    pair.value.zeroize();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod nested_tests;
