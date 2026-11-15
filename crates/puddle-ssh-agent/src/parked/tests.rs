// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn an_abandoned_request_parks_its_endpoint_until_it_finishes() {
    let path = Path::new("/parked-test/abandoned");
    let request = Request::new(path);
    assert!(!is_parked(path));
    request.abandoned();
    assert!(is_parked(path));
    assert!(!is_parked(Path::new("/parked-test/other")));
    request.finished();
    assert!(!is_parked(path));
}

#[test]
fn a_request_that_finished_before_the_caller_gave_up_parks_nothing() {
    let path = Path::new("/parked-test/late");
    let request = Request::new(path);
    request.finished();
    request.abandoned();
    assert!(!is_parked(path));
}

#[test]
fn a_request_nobody_abandoned_never_shows_as_parked() {
    let path = Path::new("/parked-test/waited");
    let request = Request::new(path);
    request.finished();
    assert!(!is_parked(path));
}
