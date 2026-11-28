// SPDX-License-Identifier: AGPL-3.0-or-later
//! Capture of one token answer: which tokens it carries, the stand-ins the tool gets in their
//! place, and the proxy's rewriter for the answer.

use super::refusal::{holds_unnamed_token, is_uuid, refusal_for};
use super::store::earlier_kept;
use super::*;

/// A token found in an answer: its field, the real token, its stand-in and whether that is new.
pub(super) type Planned = (&'static Field, Zeroizing<String>, String, bool);

impl Logins {
    /// Captures one token answer: keeps the real tokens, and gives back the body with stand-ins
    /// in their place. `Ok(None)`: nothing in the answer to capture.
    async fn capture(
        &self,
        route: (&'static Profile, &'static Endpoint),
        body: &[u8],
        content_type: Option<&str>,
    ) -> Result<Option<Bytes>, Problem> {
        let (profile, endpoint) = route;
        let mut parsed =
            TokenBody::parse(content_type, body).map_err(|_| Problem::UnexpectedAnswer)?;
        let mut fields = parsed.fields();
        // An identifier in a field named like a credential (a `token_uuid`) is no credential.
        fields.retain(|field| {
            !matches!(field.kind, FieldKind::Text(_))
                || !parsed
                    .text_at(&field.path)
                    .is_some_and(|text| is_uuid(&text))
        });
        if let Some(field) = holds_unnamed_token(endpoint, &fields) {
            // The name only, never the value: it is what lets the profile name the field.
            tracing::warn!(
                workspace = %self.inner.workspace,
                profile = profile.id,
                field = %field.path,
                "a sign-in answer carries a token in a field the profile does not name"
            );
            return Err(Problem::UnnamedToken);
        }
        let mut found: Vec<(&'static Field, Zeroizing<String>)> = Vec::new();
        for field in endpoint.fields {
            // The first field that takes the token is the one used for its answer field. Fields
            // with an audience share a path on purpose (one per resource): `by_audience` picks.
            if field.audience == Audience::Any
                && found
                    .iter()
                    .any(|(taken, _)| taken.path == field.path && taken.audience == Audience::Any)
            {
                continue;
            }
            let Some(text) = parsed.text_at(field.path).filter(|text| !text.is_empty()) else {
                continue;
            };
            if field.only_prefixed && !field.prefixes.iter().any(|p| text.starts_with(p)) {
                continue;
            }
            found.push((field, Zeroizing::new(text.into_owned())));
        }
        if found.is_empty() {
            // A pending device code, an error, a refusal: nothing to keep.
            return Ok(None);
        }
        let found = by_audience(found, &parsed);
        if found.is_empty() {
            // The answer has a token that no field of the profile takes: it must not pass as if
            // it were kept.
            return Err(Problem::UnusableToken);
        }
        if parsed
            .text("token_type")
            .is_some_and(|kind| kind.eq_ignore_ascii_case("dpop"))
        {
            return Err(Problem::BoundToken);
        }
        if found
            .iter()
            .any(|(field, real)| !shape::usable_for(field, real))
        {
            return Err(Problem::UnusableToken);
        }
        let mut one_at_a_time = Some(Arc::clone(&self.inner.capture).lock_owned().await);
        let planned = self.plan(profile, found)?;
        // The credential store first: a token that was not kept is not replaced in the answer.
        self.keep_stored(profile, &planned, &mut one_at_a_time)
            .await?;
        for (field, real, stand_in, is_new) in &planned {
            let slot = (field.slot(), field.scope);
            // A refresh gives the stand-in a new token without a moment in which it has none; a
            // slot the registry has lost (a forgotten login) is made again.
            let refreshed = !is_new
                && self
                    .inner
                    .registry
                    .set_real(stand_in, SecretValue::new(real.as_str()))
                    == Ok(true);
            if !refreshed {
                let keep_earlier = if shape::renews(real, field) {
                    earlier_kept(field)
                } else {
                    0
                };
                self.install(profile, slot, stand_in, real, keep_earlier)
                    .map_err(|_| Problem::UnusableToken)?;
            }
            parsed.set_text_at(field.path, stand_in);
            tracing::info!(
                workspace = %self.inner.workspace,
                profile = profile.id,
                role = field.role.key(),
                new = *is_new,
                "kept a login's token and gave the workspace a stand-in"
            );
        }
        Ok(Some(Bytes::copy_from_slice(&parsed.render())))
    }

    /// The stand-in each found token gets, and whether it is new: the slot's own for a token that
    /// keeps it, the one an earlier field of the same answer gave the same token, or a new one.
    fn plan(
        &self,
        profile: &'static Profile,
        found: Vec<(&'static Field, Zeroizing<String>)>,
    ) -> Result<Vec<Planned>, Problem> {
        let mut planned: Vec<Planned> = Vec::new();
        for (field, real) in found {
            // Two fields of one answer with the same token in one slot (a registry answers with
            // `token` and `access_token`) give it one stand-in.
            let in_answer = planned
                .iter()
                .find(|(taken, kept, _, _)| {
                    taken.slot() == field.slot() && kept.as_str() == real.as_str()
                })
                .map(|(_, _, stand_in, _)| stand_in.clone());
            // A session token's fields and a JWT's claims hold its expiry, so a new token gets a
            // new stand-in; any other token keeps the slot's.
            let existing = in_answer.or_else(|| {
                self.slots()
                    .get(&(profile.id, field.slot()))
                    .cloned()
                    .filter(|_| !shape::renews(&real, field))
            });
            let planned_stand_in = match existing {
                Some(stand_in) => (stand_in, false),
                // The system had no random numbers, or the token cannot be copied: either way
                // there is no stand-in to give.
                None => (
                    shape::stand_in_for(&real, field).map_err(|_| Problem::UnusableToken)?,
                    true,
                ),
            };
            planned.push((field, real, planned_stand_in.0, planned_stand_in.1));
        }
        Ok(planned)
    }
}

/// The fields among `found` that this answer is for: the ones with no condition, the first
/// [`Audience::Named`] field of each path whose resource the answer names, and the
/// [`Audience::Unnamed`] ones of a path no named field took.
fn by_audience<'f>(
    found: Vec<(&'f Field, Zeroizing<String>)>,
    parsed: &TokenBody,
) -> Vec<(&'f Field, Zeroizing<String>)> {
    let answer_hints = {
        let mut text = String::new();
        for name in ["scope", "resource"] {
            if let Some(value) = parsed.text(name) {
                text.push_str(&value);
                text.push(' ');
            }
        }
        text
    };
    let mut named: BTreeSet<&str> = BTreeSet::new();
    let mut taken = Vec::new();
    let mut unnamed = Vec::new();
    for (field, real) in found {
        match field.audience {
            Audience::Any => taken.push((field, real)),
            Audience::Named(needles) => {
                if named.contains(field.path) {
                    continue;
                }
                let mut hints = answer_hints.clone();
                hints.push_str(&shape::audience_of(&real).unwrap_or_default());
                let hints = hints.to_ascii_lowercase();
                if needles.iter().any(|needle| hints.contains(needle)) {
                    named.insert(field.path);
                    taken.push((field, real));
                }
            }
            Audience::Unnamed => unnamed.push((field, real)),
        }
    }
    taken.extend(unnamed.into_iter().filter(|(f, _)| !named.contains(f.path)));
    taken
}

/// The answer of one token endpoint, being captured.
pub(super) struct Capture {
    pub(super) logins: Logins,
    pub(super) profile: &'static Profile,
    pub(super) endpoint: &'static Endpoint,
}

impl Capture {
    /// Tells the user the login was not kept; the tool then gets the refusal, not the answer.
    fn refuse(&self, problem: Problem) -> InjectRefusal {
        self.logins.notify(self.profile, problem);
        refusal_for(self.profile, problem)
    }
}

impl AnswerRewriter for Capture {
    fn wants(&mut self, head: &AnswerHead<'_>) -> Want {
        // Only a success answer has tokens; an error answer is the tool's to read.
        if !(200..300).contains(&head.status) || head.status == 204 {
            return Want::Pass;
        }
        let identity = head
            .content_encoding
            .is_none_or(|e| e.trim().is_empty() || e.trim().eq_ignore_ascii_case("identity"));
        let small = head
            .content_length
            .is_none_or(|n| n <= MAX_EXCHANGE_BODY as u64);
        if TokenBody::reads(head.content_type) && identity && small {
            return Want::Read;
        }
        Want::Refuse(self.refuse(Problem::UnexpectedAnswer))
    }

    fn too_large(&mut self) -> Rewrite {
        Rewrite::Refuse(self.refuse(Problem::UnexpectedAnswer))
    }

    fn rewrite<'a>(
        &'a mut self,
        head: &'a AnswerHead<'a>,
        body: &'a [u8],
    ) -> puddle_proxy::BoxFuture<'a, Rewrite> {
        Box::pin(async move {
            match self
                .logins
                .capture((self.profile, self.endpoint), body, head.content_type)
                .await
            {
                Ok(Some(replaced)) => Rewrite::Replace(replaced),
                Ok(None) => Rewrite::Keep,
                Err(problem) => Rewrite::Refuse(self.refuse(problem)),
            }
        })
    }
}
