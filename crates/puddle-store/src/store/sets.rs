// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rule sets and System managed in the store (`docs/spec/rules.md` §7, R-36 to R-43): the sets
//! the user makes, the switches of every set, the built-in sets' update record, and the reasons
//! behind the System managed hosts.

mod index;
mod switches;
mod system;
mod user_sets;
mod views;

pub use index::parse_rule_set;
pub(super) use index::{load_index, note_built_in_changes};
pub(super) use switches::require_on;
pub use system::{SystemHost, SystemPlan};
pub(super) use user_sets::require_user_set;
pub use views::{ENTRY_PAGE, EntryPage, RuleSetEntryInfo, RuleSetInfo};
