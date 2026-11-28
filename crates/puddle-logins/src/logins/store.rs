// SPDX-License-Identifier: AGPL-3.0-or-later
//! The credential store side of a workspace's logins: the calls on the store with their time
//! limit, what the start loads, what is listed as kept, forgotten and put back.

use super::capture::Planned;
use super::*;

/// What reading a profile's slots from the store found.
#[derive(Default)]
struct Read {
    /// Some slot holds a login.
    kept: bool,
    /// A login was there and cannot be used.
    unreadable: bool,
    /// A call did not answer.
    store_down: bool,
}

/// How many earlier stand-ins of a slot stay swapped. A session token lasts about half an hour
/// and is exchanged again before it ends, so the tool never holds more than the last two.
pub(crate) const RETIRED_KEPT: usize = 3;

/// How many earlier stand-ins of a [`Keep::Memory`] slot stay swapped. A registry mints a token
/// per repository for each pull and the client uses it for that pull's minutes, so many pulls at
/// once (a compose file's images) each hold one; the bound keeps a guest that asks again and
/// again from growing the registry.
pub(crate) const MEMORY_KEPT: usize = 64;

/// How many earlier stand-ins of `field`'s slot stay swapped when a new one replaces them.
pub(super) const fn earlier_kept(field: &Field) -> usize {
    match field.keep {
        Keep::Memory => MEMORY_KEPT,
        _ => RETIRED_KEPT,
    }
}

impl Logins {
    /// Runs a blocking call on the credential store off the async threads, with a time limit.
    async fn store<T: Send + 'static>(
        &self,
        call: impl FnOnce(&Vault) -> Result<T, VaultError> + Send + 'static,
    ) -> Result<T, VaultError> {
        let vault = self.inner.vault.clone();
        let limit = Duration::from_millis(self.inner.store_timeout_ms.load(Ordering::Relaxed));
        let gate = Arc::clone(&self.inner.store_gate);
        let run = async move {
            let turn = gate.lock_owned().await;
            tokio::task::spawn_blocking(move || {
                let result = call(&vault);
                drop(turn);
                result
            })
            .await
        };
        match tokio::time::timeout(limit, run).await {
            Ok(Ok(result)) => result,
            // The call panicked: the store is not usable now.
            Ok(Err(_)) => Err(VaultError::Unavailable),
            Err(_) => {
                self.inner.store_timeouts.fetch_add(1, Ordering::Relaxed);
                Err(VaultError::Unavailable)
            }
        }
    }

    /// Waits until the store call that was given up on has returned (ten times the store timeout
    /// at most), so what it wrote is in the store before the caller puts the old entries back.
    async fn settle_store(&self) {
        let limit = Duration::from_millis(self.inner.store_timeout_ms.load(Ordering::Relaxed))
            .saturating_mul(10);
        // Timing out is fine: the put back then runs behind the late write and may not land.
        let _ = tokio::time::timeout(limit, self.inner.store_gate.lock()).await;
    }

    /// Puts a pair into the registry and the slots: the stand-in the workspace holds and the real
    /// token behind it, swapped toward the hosts `scope` names. The slot's stand-in it replaces
    /// stays swapped as one of the last `keep_earlier` (none: it goes at once). The registry is the
    /// one place that checks a stand-in and a real value; what it refuses is the error.
    pub(super) fn install(
        &self,
        profile: &'static Profile,
        (slot, scope): (Slot, Scope),
        stand_in: &str,
        real: &str,
        keep_earlier: usize,
    ) -> Result<(), StandInError> {
        let entry = StandIn::new(
            StandInOrigin::CapturedLogin,
            &profile.slot_name_of(slot),
            stand_in,
            SecretValue::new(real),
            profile.hosts_for(scope),
        )?;
        let old = self.slots().insert((profile.id, slot), stand_in.to_owned());
        if let Some(old) = old {
            if keep_earlier > 0 {
                self.retire(profile, slot, old, keep_earlier);
            } else {
                self.inner.registry.remove(&old);
            }
        }
        self.inner.registry.insert(entry)
    }

    /// Keeps `old` swapped as an earlier stand-in of the slot, and stops swapping the oldest ones
    /// beyond `limit`.
    fn retire(&self, profile: &Profile, slot: Slot, old: String, limit: usize) {
        let mut retired = self
            .inner
            .retired
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let queue = retired.entry((profile.id, slot)).or_default();
        queue.push_back(old);
        while queue.len() > limit {
            if let Some(oldest) = queue.pop_front() {
                self.inner.registry.remove(&oldest);
            }
        }
    }

    /// Reads the logins kept for the workspace and registers their stand-ins, at the workspace's
    /// start: the workspace's disk still holds the stand-ins its tool was given.
    pub async fn load(&self) {
        // With capture off nothing is registered and a login kept before is not used until
        // capture is on again. The store is only asked whether one is there, because the
        // workspace's disk still holds its stand-ins, which are now refused: that is said once.
        if !self.inner.enabled {
            self.tell_about_kept_logins().await;
            return;
        }
        let _one_at_a_time = self.inner.capture.lock().await;
        let mut unreadable: Vec<&'static Profile> = Vec::new();
        let mut store_down = false;
        let mut kept: BTreeSet<String> = BTreeSet::new();
        for profile in self.inner.profiles {
            // One call that did not answer is enough to know; the next would wait as long.
            if store_down {
                continue;
            }
            let read = self.read_missing(profile).await;
            if read.kept {
                kept.insert(profile.id.to_owned());
            }
            if read.unreadable {
                unreadable.push(profile);
            }
            store_down = read.store_down;
        }
        if store_down {
            // The first exchange of each service asks again.
            self.unread()
                .extend(self.inner.profiles.iter().map(|p| p.id));
        }
        // Only an answer from every slot says what is kept.
        if !store_down {
            self.inner.marker.write_off_thread(kept).await;
        }
        // With the store down nothing can be kept either, so every service is told once, with
        // the words for a login that is not protected.
        let down: Vec<&'static Profile> = if store_down {
            self.inner.profiles.iter().collect()
        } else {
            Vec::new()
        };
        for (list, problem) in [
            (down, Problem::StoreUnavailable),
            (unreadable, Problem::Unreadable),
        ] {
            let mut seen: Vec<&str> = Vec::new();
            for profile in list {
                if !seen.contains(&profile.id) {
                    seen.push(profile.id);
                    self.notify(profile, problem);
                }
            }
        }
    }

    /// Reads the slots of `profile` that hold no stand-in yet and registers what the store keeps.
    /// Stops at the first call the store does not answer.
    async fn read_missing(&self, profile: &'static Profile) -> Read {
        let mut read = Read::default();
        for slot in profile.slots() {
            if self.slots().contains_key(&(profile.id, slot.0)) {
                read.kept = true;
                continue;
            }
            match self.store(move |vault| vault.read(profile, slot.0)).await {
                Ok(Some(saved)) => {
                    read.kept = true;
                    if self
                        .install(profile, slot, &saved.stand_in, &saved.real, 0)
                        .is_err()
                    {
                        read.unreadable = true;
                    }
                }
                Ok(None) => {}
                Err(VaultError::Damaged) => {
                    read.kept = true;
                    read.unreadable = true;
                }
                Err(VaultError::Unavailable) => {
                    read.store_down = true;
                    break;
                }
            }
        }
        read
    }

    pub(super) fn unread(&self) -> std::sync::MutexGuard<'_, BTreeSet<&'static str>> {
        self.inner
            .unread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Asks the store again for `profile`'s logins, the one time after a start where it did not
    /// answer. It runs before the exchange's request is read, under the capture lock, so a
    /// sign-in's capture does not write over a login kept earlier that is still good.
    pub(crate) async fn read_again(&self, profile: &'static Profile) {
        let _one_at_a_time = self.inner.capture.lock().await;
        let read = self.read_missing(profile).await;
        if read.kept {
            self.inner.marker.note_off_thread(profile.id, true).await;
        }
        if read.unreadable {
            self.notify(profile, Problem::Unreadable);
        }
    }

    /// With capture off: tells the user, once per service, that Puddle still holds a login it
    /// kept earlier. The marker says which services those are without asking the credential
    /// store, which can wait on a locked keyring at every start. With no marker yet (a workspace
    /// from before it, or one it could not be written for) the store is asked once, the answer
    /// is written to the marker, and a store that does not answer says nothing: with capture off
    /// there is nothing to protect, and one call that did not answer is enough to know.
    async fn tell_about_kept_logins(&self) {
        if let Some(kept) = self.inner.marker.read_off_thread().await {
            for profile in self.inner.profiles.iter().filter(|p| kept.contains(p.id)) {
                self.notify(profile, Problem::CaptureOff);
            }
            return;
        }
        // A store that does not answer says nothing: with capture off there is nothing to protect.
        let Some(kept) = self.kept_in_store().await else {
            return;
        };
        for profile in self.inner.profiles.iter().filter(|p| kept.contains(p.id)) {
            self.notify(profile, Problem::CaptureOff);
        }
        self.inner.marker.write_off_thread(kept).await;
    }

    /// The services the credential store holds a login for, or `None` when it did not answer
    /// (one call that did not answer is enough to know; the next would wait as long).
    async fn kept_in_store(&self) -> Option<BTreeSet<String>> {
        let mut kept: BTreeSet<String> = BTreeSet::new();
        for profile in self.inner.profiles {
            for (slot, _) in profile.slots() {
                match self.store(move |vault| vault.holds(profile, slot)).await {
                    Ok(true) => {
                        kept.insert(profile.id.to_owned());
                        break;
                    }
                    Ok(false) => {}
                    Err(_) => return None,
                }
            }
        }
        Some(kept)
    }

    /// The services Puddle keeps a login for in this workspace, for the user to look at: names and
    /// times, never a value. The marker answers when it is known, so opening the list never waits
    /// on a locked keyring; with no marker the store is asked once and the answer is written to
    /// the marker. A store that does not answer gives an empty list that says so
    /// ([`KeptList::store_answered`]).
    pub async fn kept_list(&self) -> KeptList {
        let since = match self.inner.marker.read_since_off_thread().await {
            Some(since) => since,
            None => match self.kept_in_store().await {
                Some(kept) => {
                    self.inner.marker.write_off_thread(kept.clone()).await;
                    // What the marker now says; where it cannot be written, no time is known.
                    self.inner
                        .marker
                        .read_since_off_thread()
                        .await
                        .unwrap_or_else(|| kept.into_iter().map(|id| (id, None)).collect())
                }
                None => {
                    return KeptList {
                        logins: Vec::new(),
                        store_answered: false,
                    };
                }
            },
        };
        let logins = self
            .inner
            .profiles
            .iter()
            .filter_map(|profile| {
                since.get(profile.id).map(|since_ms| KeptLogin {
                    profile: profile.id,
                    name: profile.name,
                    since_ms: *since_ms,
                })
            })
            .collect();
        KeptList {
            logins,
            store_answered: true,
        }
    }

    /// What the marker says is kept, as a sorted list (tests).
    #[cfg(test)]
    pub(crate) fn inner_marker_kept(&self) -> Option<Vec<String>> {
        self.inner
            .marker
            .read()
            .map(|ids| ids.into_iter().collect())
    }

    /// The logins Puddle holds for this workspace (the ones registered since its start), without
    /// any value.
    #[must_use]
    pub fn kept(&self) -> Vec<Kept> {
        let slots = self.slots();
        self.inner
            .profiles
            .iter()
            .filter_map(|profile| {
                let mut roles: Vec<Role> = profile
                    .slots()
                    .into_iter()
                    .filter(|(slot, _)| slots.contains_key(&(profile.id, *slot)))
                    .map(|(slot, _)| slot.role)
                    .collect();
                roles.dedup();
                (!roles.is_empty()).then_some(Kept {
                    profile: profile.id,
                    name: profile.name,
                    roles,
                })
            })
            .collect()
    }

    /// Forgets `profile`'s login for this workspace: its stand-ins stop being swapped (the tool
    /// will ask for a new sign-in) and its real tokens are deleted from the credential store.
    /// `false` when the profile is not one of this workspace's.
    ///
    /// # Errors
    /// [`ForgetError::Store`] when the credential store did not delete a token; the stand-ins are
    /// already gone, and trying again deletes what is left.
    pub async fn forget(&self, profile_id: &str) -> Result<bool, ForgetError> {
        let Some(profile) = self
            .inner
            .profiles
            .iter()
            .find(|p| p.id == profile_id || p.name == profile_id)
        else {
            return Ok(false);
        };
        let _one_at_a_time = self.inner.capture.lock().await;
        let mut failed = false;
        for (slot, _) in profile.memory_slots() {
            self.unregister(profile, slot);
        }
        for (slot, _) in profile.slots() {
            self.unregister(profile, slot);
            failed |= self
                .store(move |vault| vault.delete(profile, slot))
                .await
                .is_err();
        }
        if failed {
            Err(ForgetError::Store)
        } else {
            self.inner.marker.note_off_thread(profile.id, false).await;
            Ok(true)
        }
    }

    /// Stops swapping the stand-ins of `slot`, its current one and the earlier ones.
    fn unregister(&self, profile: &Profile, slot: Slot) {
        let current = self.slots().remove(&(profile.id, slot));
        if let Some(stand_in) = current {
            self.inner.registry.remove(&stand_in);
        }
        let earlier = self
            .inner
            .retired
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&(profile.id, slot));
        for stand_in in earlier.into_iter().flatten() {
            self.inner.registry.remove(&stand_in);
        }
    }

    /// Deletes every token kept for `workspace`, when the workspace is deleted. Blocking. Returns
    /// how many deletions the store refused (the rest are gone).
    #[must_use]
    pub fn delete_workspace(
        store: &Arc<dyn SecretStore>,
        workspace: &WorkspaceId,
        profiles: &[Profile],
    ) -> usize {
        crate::vault::saved_login_ids(workspace, profiles)
            .iter()
            .filter(|id| store.delete(id).is_err())
            .count()
    }

    /// Writes the tokens of `planned` that are kept in the credential store, with their
    /// stand-ins, and notes the login in the marker. `lock` holds the capture lock; a put back that
    /// has to wait for a write that timed out takes it into a task, so the tool is answered first.
    pub(super) async fn keep_stored(
        &self,
        profile: &'static Profile,
        planned: &[Planned],
        lock: &mut Option<tokio::sync::OwnedMutexGuard<()>>,
    ) -> Result<(), Problem> {
        // One write per slot, with its last field's token, as the registry gets the last one too.
        let mut writes: Vec<(Slot, &Zeroizing<String>, &String)> = Vec::new();
        for (field, real, stand_in, _) in planned.iter().rev() {
            if field.keep == Keep::Stored && !writes.iter().any(|(slot, ..)| *slot == field.slot())
            {
                writes.push((field.slot(), real, stand_in));
            }
        }
        writes.reverse();
        let slots: Vec<Slot> = writes.iter().map(|(slot, ..)| *slot).collect();
        // An answer with several tokens is kept whole or not at all: what the store held before
        // goes back when a later token cannot be written, so it never holds a pair that did not
        // come from one answer. One token needs no copy: its write is the whole change. Only the
        // slots written before the failure go back, and the failed one too when its write timed
        // out: that write may still land, so the put back waits for it first. A write the store
        // refused wrote nothing, and putting its slot back would only wait out a locked store's
        // timeout again.
        let before = if slots.len() > 1 {
            self.saved(profile, &slots).await?
        } else {
            Vec::new()
        };
        for (written, (slot, real, stand_in)) in writes.into_iter().enumerate() {
            let (stand_in, real) = (stand_in.clone(), real.clone());
            let timeouts = self.inner.store_timeouts.load(Ordering::Relaxed);
            if self
                .store(move |vault| vault.write(profile, slot, &stand_in, &real))
                .await
                .is_err()
            {
                if self.inner.store_timeouts.load(Ordering::Relaxed) == timeouts {
                    self.put_back(profile, before.into_iter().take(written))
                        .await;
                    return Err(Problem::StoreUnavailable);
                }
                // The write timed out and may still land. With nothing to put back there is
                // nothing to wait for; otherwise the put back waits for it in a task that keeps
                // the capture lock, and the tool gets its answer now.
                let back: Vec<_> = before.into_iter().take(written + 1).collect();
                let held = lock.take();
                if let (false, Some(held)) = (back.is_empty(), held) {
                    let this = self.clone();
                    tokio::spawn(async move {
                        this.settle_store().await;
                        this.put_back(profile, back).await;
                        drop(held);
                    });
                }
                return Err(Problem::StoreUnavailable);
            }
        }
        if !slots.is_empty() {
            self.inner.marker.note_off_thread(profile.id, true).await;
        }
        Ok(())
    }

    /// What the store holds in each of `slots` of `profile` (`None`: nothing, or nothing
    /// readable), in their order.
    async fn saved(
        &self,
        profile: &'static Profile,
        slots: &[Slot],
    ) -> Result<Vec<(Slot, Option<Saved>)>, Problem> {
        let mut saved = Vec::with_capacity(slots.len());
        for &slot in slots {
            match self.store(move |vault| vault.read(profile, slot)).await {
                Ok(entry) => saved.push((slot, entry)),
                Err(VaultError::Damaged) => saved.push((slot, None)),
                Err(VaultError::Unavailable) => return Err(Problem::StoreUnavailable),
            }
        }
        Ok(saved)
    }

    /// Writes `saved` back into the store, as far as the store lets it: an entry that was
    /// empty is deleted. A write the store refuses is only logged; the capture fails either way.
    async fn put_back(
        &self,
        profile: &'static Profile,
        saved: impl IntoIterator<Item = (Slot, Option<Saved>)>,
    ) {
        for (slot, entry) in saved {
            let result = self
                .store(move |vault| match entry {
                    Some(entry) => vault.write(profile, slot, &entry.stand_in, &entry.real),
                    None => vault.delete(profile, slot),
                })
                .await;
            if result.is_err() {
                tracing::warn!(
                    workspace = %self.inner.workspace,
                    profile = profile.id,
                    "a login's earlier token could not be put back in the credential store"
                );
            }
        }
    }
}
