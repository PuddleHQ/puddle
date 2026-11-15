// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn unswapped(ids: &[String]) -> Swapped {
    Swapped {
        unswapped: ids.to_vec(),
        ..Swapped::default()
    }
}

#[test]
fn the_record_lists_each_unswapped_stand_in_once_and_stops_at_the_cap() {
    let host = Host::parse_normalised("bound.test").unwrap();
    let mut outcome = Outcome::default();
    let ids: Vec<String> = (0..MAX_UNSWAPPED_STAND_INS + 5)
        .map(|n| format!("stand-in:secret:S{n}"))
        .collect();
    outcome.record_stand_ins(&host, &unswapped(&ids[..3]));
    outcome.record_stand_ins(&host, &unswapped(&ids[1..4]));
    assert_eq!(outcome.unswapped_stand_ins, ids[..4]);
    outcome.record_stand_ins(&host, &unswapped(&ids));
    assert_eq!(outcome.unswapped_stand_ins, ids[..MAX_UNSWAPPED_STAND_INS]);
}

#[test]
fn the_record_lists_each_scrubbed_stand_in_once_and_stops_at_the_cap() {
    let ids: Vec<String> = (0..MAX_UNSWAPPED_STAND_INS + 5)
        .map(|n| format!("stand-in:secret:S{n}"))
        .collect();
    let notes = |ids: &[String]| {
        let notes = ScrubNotes::default();
        for id in ids {
            notes.note(id);
        }
        notes
    };
    let mut outcome = Outcome::default();
    outcome.record_scrubbed(&notes(&ids[..3]));
    outcome.record_scrubbed(&notes(&ids[1..4]));
    assert_eq!(outcome.scrubbed_stand_ins, ids[..4]);
    outcome.record_scrubbed(&notes(&ids));
    assert_eq!(outcome.scrubbed_stand_ins, ids[..MAX_UNSWAPPED_STAND_INS]);
    assert!(outcome.refused.is_none());
}
