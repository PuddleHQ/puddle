// SPDX-License-Identifier: AGPL-3.0-or-later
//! One test binary for every area below, so the shared support code compiles and links once.

mod audit_retention;
mod audit_row_size;
mod events_and_audit_query;
mod golden_database;
mod identities_spec;
mod rule_sets_spec;
mod rules_spec;
mod users_spec;
mod workspace_create_allow_spec;
