// SPDX-License-Identifier: AGPL-3.0-or-later
//! One list out of every credential's: repositories reached by more than one identity appear
//! once with all of them, filtered by what the user typed and cut to a page. The Identities tab
//! and the create form both read this, so they agree on order and spelling.

use std::collections::HashMap;

use puddle_store::IdentityId;

use crate::model::{Repository, SourceList};

/// What to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    /// Words that must all appear in the repository's full name, whatever the case; empty for
    /// everything.
    pub text: String,
    /// Only repositories this identity reaches.
    pub only: Option<IdentityId>,
    /// Repositories to skip.
    pub offset: usize,
    /// Repositories to return.
    pub limit: usize,
}

/// A repository with the identities that reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The repository, as the first list that had it spells it.
    pub repository: Repository,
    /// The identities whose credentials list it, in the order of their lists.
    pub identities: Vec<IdentityId>,
}

/// A page of repositories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// How many repositories match, before the page is cut.
    pub total: usize,
    /// The page, by full name.
    pub repos: Vec<Found>,
}

/// The repositories of `lists` that match `query`, one entry each, by full name.
#[must_use]
pub fn page(lists: &[SourceList], query: &Query) -> Page {
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut found: Vec<Found> = Vec::new();
    for list in lists {
        for repository in list.repos.iter() {
            if let Some(&at) = index.get(&repository.key()) {
                if let Some(entry) = found.get_mut(at)
                    && !entry.identities.contains(&list.identity)
                {
                    entry.identities.push(list.identity);
                }
            } else {
                index.insert(repository.key(), found.len());
                found.push(Found {
                    repository: repository.clone(),
                    identities: vec![list.identity],
                });
            }
        }
    }
    let words: Vec<String> = query
        .text
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    found.retain(|f| {
        let name = f.repository.full_name.to_lowercase();
        query.only.is_none_or(|id| f.identities.contains(&id))
            && words.iter().all(|word| name.contains(word.as_str()))
    });
    found.sort_by_cached_key(|f| {
        (
            f.repository.full_name.to_lowercase(),
            f.repository.host.clone(),
        )
    });
    Page {
        total: found.len(),
        repos: found
            .into_iter()
            .skip(query.offset)
            .take(query.limit)
            .collect(),
    }
}

#[cfg(test)]
mod tests;
