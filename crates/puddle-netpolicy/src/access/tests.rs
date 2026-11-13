// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_settings::{GlobalSettings, WorkspaceSettings, resolve};

use super::*;

#[test]
fn none_is_the_default_and_everything_off() {
    assert_eq!(LocalAccess::default(), LocalAccess::NONE);
    for c in LocalCategory::ALL {
        assert!(!LocalAccess::NONE.is_on(c));
    }
    assert!(!LocalAccess::NONE.wildcards_reach_local());
}

#[test]
fn unreadable_settings_are_none_that_says_so() {
    assert!(LocalAccess::UNREADABLE.is_unreadable());
    assert!(!LocalAccess::NONE.is_unreadable());
    assert_ne!(LocalAccess::UNREADABLE, LocalAccess::NONE);
    for c in LocalCategory::ALL {
        assert!(!LocalAccess::UNREADABLE.is_on(c));
    }
    assert!(!LocalAccess::UNREADABLE.wildcards_reach_local());
}

#[test]
fn toggles_are_independent() {
    for c in LocalCategory::ALL {
        let a = LocalAccess::NONE.with_toggle(c, true);
        for other in LocalCategory::ALL {
            assert_eq!(a.is_on(other), other == c, "{c} vs {other}");
        }
        assert_eq!(a.with_toggle(c, false), LocalAccess::NONE);
    }
}

#[test]
fn from_effective_follows_the_resolved_settings() {
    assert_eq!(
        LocalAccess::from_effective(&Effective::DEFAULTS),
        LocalAccess::NONE
    );
    let mut g = GlobalSettings::default();
    g.workspace_defaults.local_toggles.private = Some(true);
    g.workspace_defaults.local_toggles.metadata = Some(true);
    g.workspace_defaults.wildcards_reach_local = Some(true);
    let mut s = WorkspaceSettings::default();
    s.overrides.local_toggles.metadata = Some(false);
    let a = LocalAccess::from_effective(&resolve(&g, Some(&s)));
    assert!(a.is_on(LocalCategory::Private));
    assert!(!a.is_on(LocalCategory::Metadata));
    assert!(!a.is_on(LocalCategory::Loopback));
    assert!(a.wildcards_reach_local());
}

#[test]
fn sources_fixed_and_per_workspace() {
    let workspace = WorkspaceId::new("box").unwrap();
    let fixed = LocalAccess::NONE.with_toggle(LocalCategory::Loopback, true);
    assert_eq!(fixed.local_access(&workspace), fixed);
    let per = |s: &WorkspaceId| {
        LocalAccess::NONE.with_toggle(LocalCategory::Private, s.as_str() == "box")
    };
    assert!(per.local_access(&workspace).is_on(LocalCategory::Private));
    let other = WorkspaceId::new("other").unwrap();
    assert!(!per.local_access(&other).is_on(LocalCategory::Private));
}
