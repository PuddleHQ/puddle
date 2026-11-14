// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use super::*;

fn host(text: &str) -> Host {
    Host::parse_normalised(text).unwrap()
}

#[test]
fn learning_from_rejections_is_on_until_turned_off_and_can_be_turned_on_again() {
    let learned = Arc::new(PassThrough::new());
    let on = Interception::new(Arc::clone(&learned));
    assert!(on.is_learning());
    let off = on.learn_from_rejections(false);
    assert!(!off.is_learning());
    let again = off.learn_from_rejections(true);
    assert!(again.is_learning());
    assert!(Arc::ptr_eq(again.learned(), &learned), "same list");
}

#[test]
fn a_learned_host_is_remembered_until_its_time_is_up() {
    let list = PassThrough::with_limits(Duration::from_secs(3600), 8);
    assert!(list.learn(&host("pinned.example")));
    assert!(!list.learn(&host("pinned.example")), "renewing is not new");
    assert!(list.contains(&host("pinned.example")));
    assert!(!list.contains(&host("other.example")));
    assert_eq!(list.list().len(), 1);
    list.age(&host("pinned.example"), Duration::from_secs(3599));
    assert!(list.contains(&host("pinned.example")), "a second left");
    list.age(&host("pinned.example"), Duration::from_secs(2));
    assert!(!list.contains(&host("pinned.example")));
    assert_eq!(list.list(), []);
    assert!(
        list.learn(&host("pinned.example")),
        "new again after expiry"
    );
}

#[test]
fn a_full_list_drops_its_oldest_host_and_a_forgotten_host_is_gone() {
    let list = PassThrough::with_limits(Duration::from_secs(60), 2);
    list.learn(&host("a.example"));
    list.age(&host("a.example"), Duration::from_secs(10));
    list.learn(&host("b.example"));
    list.age(&host("b.example"), Duration::from_secs(5));
    list.learn(&host("c.example"));
    assert!(!list.contains(&host("a.example")));
    assert!(list.contains(&host("b.example")) && list.contains(&host("c.example")));
    list.forget(&host("b.example"));
    assert!(!list.contains(&host("b.example")));
    assert_eq!(list.list().len(), 1);
}

#[test]
fn forgetting_says_whether_the_host_was_remembered_and_clearing_counts_the_live_ones() {
    let list = PassThrough::with_limits(Duration::from_secs(60), 8);
    list.learn(&host("a.example"));
    list.learn(&host("b.example"));
    list.learn(&host("c.example"));
    list.age(&host("c.example"), Duration::from_secs(61));
    assert!(list.forget(&host("a.example")));
    assert!(!list.forget(&host("a.example")), "already gone");
    assert!(
        !list.forget(&host("c.example")),
        "expired is not remembered"
    );
    assert_eq!(list.clear(), 1, "only b.example was still live");
    assert_eq!(list.list(), []);
    assert_eq!(list.clear(), 0);
}

#[test]
fn new_limits_apply_to_the_hosts_already_remembered() {
    let list = PassThrough::with_limits(Duration::from_secs(3600), 8);
    for (name, age) in [("a", 30), ("b", 20), ("c", 10)] {
        let h = host(&format!("{name}.example"));
        list.learn(&h);
        list.age(&h, Duration::from_secs(age));
    }
    list.set_limits(Duration::from_secs(3600), 2);
    assert!(!list.contains(&host("a.example")), "the oldest made room");
    assert_eq!(list.list().len(), 2);
    list.set_limits(Duration::from_secs(15), 0);
    assert_eq!(list.cap(), 1, "a cap of zero is one");
    assert_eq!(list.ttl(), Duration::from_secs(15));
    assert_eq!(
        list.list()
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>(),
        ["c.example"],
        "a shorter time expired b at once"
    );
}

#[test]
fn the_observer_hears_each_new_host_once() {
    let heard = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&heard);
    let list = PassThrough::new().observed(move |h| sink.lock().unwrap().push(h.to_string()));
    list.learn(&host("a.example"));
    list.learn(&host("a.example"));
    list.learn(&host("b.example"));
    assert_eq!(*heard.lock().unwrap(), ["a.example", "b.example"]);
    assert!(format!("{list:?}").contains("PassThrough"));
}

#[test]
fn the_defaults_are_twelve_hours_and_a_thousand_hosts() {
    let list = PassThrough::new();
    assert_eq!(list.ttl(), Duration::from_hours(12));
    assert_eq!(list.cap(), 1024);
    assert_eq!(PassThrough::with_limits(Duration::ZERO, 0).cap(), 1);
}

#[test]
fn only_a_decrypting_workspace_skips_hosts_and_it_says_why() {
    let learned = Arc::new(PassThrough::new());
    learned.learn(&host("pinned.example"));
    let never = TerminationSet::parse(["bank.example", "*.health.example"]).unwrap();
    let off = Interception::new(Arc::clone(&learned)).never_decrypt(never.clone());
    assert!(!off.is_decrypting_all());
    assert_eq!(off.skipped(&host("bank.example")), None, "switch off");
    let on = off.clone().decrypt_all(true);
    assert!(on.is_decrypting_all() && on.never().contains(&host("bank.example")));
    assert_eq!(on.skipped(&host("bank.example")), Some("never_decrypt"));
    assert_eq!(on.skipped(&host("a.health.example")), Some("never_decrypt"));
    assert_eq!(on.skipped(&host("pinned.example")), Some("learned"));
    assert_eq!(on.skipped(&host("registry.npmjs.org")), None);
    assert_eq!(
        on.skipped(&host("140.82.112.3")),
        None,
        "an IP is never decrypted"
    );
    assert!(Arc::ptr_eq(on.learned(), &learned));
}

#[test]
fn a_curated_host_is_passed_through_after_the_never_decrypt_names_and_before_learning() {
    let learned = Arc::new(PassThrough::new());
    let curated = TerminationSet::parse(["*.docker.example", "bank.example"]).unwrap();
    let never = TerminationSet::parse(["bank.example"]).unwrap();
    let on = Interception::new(Arc::clone(&learned))
        .decrypt_all(true)
        .never_decrypt(never)
        .curated(curated);
    assert_eq!(on.skipped(&host("bank.example")), Some("never_decrypt"));
    assert_eq!(on.skipped(&host("a.docker.example")), Some("curated"));
    assert_eq!(on.skipped(&host("other.example")), None);
    learned.learn(&host("a.docker.example"));
    assert_eq!(on.skipped(&host("a.docker.example")), Some("curated"));
    let off = on.clone().decrypt_all(false);
    assert_eq!(off.skipped(&host("a.docker.example")), None, "switch off");
    assert!(on.is_learning());
    assert!(!on.learn_from_rejections(false).is_learning());
}
