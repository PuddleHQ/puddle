// SPDX-License-Identifier: AGPL-3.0-or-later
//! The refusal rule: an answer that carries a token Puddle would not replace is refused, and the
//! tool is told why.

use super::*;

/// The shortest text in a token-like field that counts as a token: shorter ones are lifetimes,
/// counts and flags that happen to carry the word in their name.
const MIN_TOKEN_LEN: usize = 16;

/// Name endings of fields that carry a count, a kind or an address, not a credential.
const NOT_TOKENS: [&str; 8] = [
    "_type",
    "_in",
    "_at",
    "_url",
    "_uri",
    "_endpoint",
    "_scope",
    "_label",
];

/// Whether a field's name says it holds a credential.
fn token_like(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    if NOT_TOKENS.iter().any(|suffix| name.ends_with(suffix)) {
        return false;
    }
    name.contains("token")
        || name.contains("secret")
        || name.contains("password")
        || name.ends_with("key")
}

/// Whether the answer holds a credential Puddle would not replace: a token-like text in a field
/// the endpoint does not name (at any depth), or a named field that is repeated or structured
/// (the tool may read the one Puddle did not). A UUID is an identifier, not a credential, and is
/// removed from `fields` by the caller.
pub(super) fn holds_unnamed_token<'a>(
    endpoint: &Endpoint,
    fields: &'a [BodyField],
) -> Option<&'a BodyField> {
    let named = |field: &BodyField| endpoint.fields.iter().any(|f| f.path == field.path);
    fields.iter().find(|field| {
        if named(field) {
            let repeated = fields.iter().filter(|f| f.path == field.path).count() > 1;
            return repeated || field.kind == FieldKind::Structured;
        }
        matches!(field.kind, FieldKind::Text(n) if n >= MIN_TOKEN_LEN) && token_like(&field.name)
    })
}

/// Whether a text is a UUID (8-4-4-4-12 hex digits): an identifier a service names a token by,
/// not a secret.
pub(super) fn is_uuid(text: &str) -> bool {
    text.len() == 36
        && text.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// What the tool is told when its login is refused: the reason, and what to do.
pub(super) fn refusal_for(profile: &Profile, problem: Problem) -> InjectRefusal {
    let why = match problem {
        Problem::StoreUnavailable => {
            "the credential store is not available (locked or not running); unlock it or start it and sign in again"
        }
        Problem::UnexpectedAnswer => {
            "the sign-in answer is in a form Puddle does not read (compressed, too large or not JSON or a form)"
        }
        Problem::UnusableToken => "the token has a form Puddle cannot give a stand-in for",
        Problem::BoundToken => "the service ties its token to a key of the tool's own",
        Problem::UnnamedToken => "the answer carries a token in a field Puddle does not know",
        _ => "Puddle could not keep it",
    };
    InjectRefusal::new(
        502,
        ReasonCode::LoginNotKept,
        format!(
            "Puddle did not let the {} sign-in through: {why}. Nothing reached this workspace; Puddle's notice says what to do.",
            profile.name
        ),
    )
}

#[cfg(test)]
mod tests;
