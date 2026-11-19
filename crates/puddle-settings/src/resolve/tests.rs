// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use puddle_types::CpuCount;

#[test]
fn direct_ssh_follows_the_override_then_the_global_default() {
    let mut g = GlobalSettings::default();
    let mut s = WorkspaceSettings::default();
    g.workspace_defaults.direct_ssh = Some(true);
    let e = resolve(&g, Some(&s));
    assert!(e.direct_ssh.value);
    assert_eq!(e.direct_ssh.source, Source::Global);
    s.overrides.direct_ssh = Some(false);
    let e = resolve(&g, Some(&s));
    assert!(!e.direct_ssh.value);
    assert_eq!(e.direct_ssh.source, Source::Workspace);
}

#[test]
fn the_foreign_credential_warning_is_on_until_someone_turns_it_off() {
    let mut g = GlobalSettings::default();
    let mut s = WorkspaceSettings::default();
    let e = resolve(&g, Some(&s));
    assert!(e.warn_foreign_credentials.value);
    assert_eq!(e.warn_foreign_credentials.source, Source::Default);
    g.workspace_defaults.warn_foreign_credentials = Some(false);
    let e = resolve(&g, Some(&s));
    assert!(!e.warn_foreign_credentials.value);
    assert_eq!(e.warn_foreign_credentials.source, Source::Global);
    s.overrides.warn_foreign_credentials = Some(true);
    let e = resolve(&g, Some(&s));
    assert!(e.warn_foreign_credentials.value);
    assert_eq!(e.warn_foreign_credentials.source, Source::Workspace);
}

#[test]
fn login_capture_is_on_until_someone_turns_it_off() {
    let mut g = GlobalSettings::default();
    let mut s = WorkspaceSettings::default();
    let e = resolve(&g, Some(&s));
    assert!(e.capture_logins.value);
    assert_eq!(e.capture_logins.source, Source::Default);
    g.workspace_defaults.capture_logins = Some(false);
    let e = resolve(&g, Some(&s));
    assert!(!e.capture_logins.value);
    assert_eq!(e.capture_logins.source, Source::Global);
    s.overrides.capture_logins = Some(true);
    let e = resolve(&g, Some(&s));
    assert!(e.capture_logins.value);
    assert_eq!(e.capture_logins.source, Source::Workspace);
}

#[test]
fn nothing_set_gives_the_built_in_defaults() {
    let e = resolve(&GlobalSettings::default(), None);
    assert_eq!(e, Effective::DEFAULTS);
    assert_eq!(e.memory.value, MemoryMib::DEFAULT);
    assert!(!e.local_toggles.metadata.value);
    assert!(!e.wildcards_reach_local.value);
    assert!(
        !e.direct_ssh.value,
        "direct SSH is off until someone turns it on"
    );
    assert_eq!(e.reconnection_grace.value.secs(), 300);
    assert!(e.zoom_hotkeys.value);
    assert_eq!(e.clipboard_read.value, ClipboardRead::Ask);
    assert_eq!(
        resolve(
            &GlobalSettings::default(),
            Some(&WorkspaceSettings::default())
        ),
        Effective::DEFAULTS
    );
}

#[test]
fn effective_values_serialise_with_their_source() {
    let mut g = GlobalSettings::default();
    g.workspace_defaults.zoom_hotkeys = Some(false);
    let v = serde_json::to_value(resolve(&g, None)).unwrap();
    assert_eq!(
        v["zoom_hotkeys"],
        serde_json::json!({"value": false, "source": "global"})
    );
    assert_eq!(v["memory"]["source"], "default");
    assert_eq!(v["local_toggles"]["private"]["value"], false);
}

#[test]
fn toggles_resolve_per_category_under_their_keys() {
    for category in LocalCategory::ALL {
        let mut g = GlobalSettings::default();
        let mut s = WorkspaceSettings::default();
        // The global default turns it on; the workspace turns it off again.
        let doc = serde_json::json!({ category.key(): true });
        g.workspace_defaults.local_toggles = serde_json::from_value(doc).unwrap();
        assert_eq!(
            resolve(&g, Some(&s)).local_toggles.get(category),
            Resolved {
                value: true,
                source: Source::Global
            }
        );
        let doc = serde_json::json!({ category.key(): false });
        s.overrides.local_toggles = serde_json::from_value(doc).unwrap();
        let e = resolve(&g, Some(&s));
        assert_eq!(
            e.local_toggles.get(category),
            Resolved {
                value: false,
                source: Source::Workspace
            }
        );
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["local_toggles"][category.key()]["source"], "workspace");
        for other in LocalCategory::ALL.into_iter().filter(|o| *o != category) {
            assert_eq!(e.local_toggles.get(other).source, Source::Default);
        }
    }
}
#[test]
fn the_company_network_is_off_until_a_level_turns_it_on() {
    let c = LocalCategory::CompanyNetwork;
    assert_eq!(
        resolve(&GlobalSettings::default(), None)
            .local_toggles
            .get(c),
        Resolved {
            value: false,
            source: Source::Default
        }
    );
    // On in a workspace only: another workspace and the global value stay off.
    let mut s = WorkspaceSettings::default();
    s.overrides.local_toggles.company_network = Some(true);
    let g = GlobalSettings::default();
    assert!(resolve(&g, Some(&s)).local_toggles.get(c).value);
    assert!(
        !resolve(&g, Some(&WorkspaceSettings::default()))
            .local_toggles
            .get(c)
            .value
    );
}

fn cpus(n: u32) -> CpuCount {
    CpuCount::new(n).unwrap()
}

#[test]
fn the_built_in_cpu_default_is_four_or_half_the_cores() {
    let global = GlobalSettings::default();
    for (cores, want) in [(1, 1), (2, 1), (4, 2), (8, 4), (32, 4)] {
        let e = resolve_on_host(&global, None, cores);
        assert_eq!(e.cpus.value.get(), want, "{cores} cores");
        assert_eq!(e.cpus.source, Source::Default);
    }
}

#[test]
fn a_chosen_cpu_count_wins_over_the_default_and_is_not_capped_at_32() {
    let mut global = GlobalSettings::default();
    global.workspace_defaults.cpus = Some(cpus(6));
    let mut own = WorkspaceSettings::default();
    assert_eq!(resolve_on_host(&global, Some(&own), 16).cpus.value.get(), 6);
    own.overrides.cpus = Some(cpus(48));
    let e = resolve_on_host(&global, Some(&own), 64);
    assert_eq!((e.cpus.value.get(), e.cpus.source), (48, Source::Workspace));
}

#[test]
fn the_maximum_defaults_to_the_cores_and_lowers_any_level_but_keeps_its_source() {
    let mut global = GlobalSettings::default();
    let mut own = WorkspaceSettings::default();
    own.overrides.cpus = Some(cpus(12));
    assert_eq!(global.limits.max_cpus_on(8).get(), 8);
    let e = resolve_on_host(&global, Some(&own), 8);
    assert_eq!((e.cpus.value.get(), e.cpus.source), (8, Source::Workspace));
    assert_eq!(
        resolve_on_host(&global, Some(&own), 16).cpus.value.get(),
        12
    );

    global.limits.max_cpus = Some(cpus(2));
    let e = resolve_on_host(&global, Some(&own), 64);
    assert_eq!(e.cpus.value.get(), 2, "a set maximum beats the cores");
    assert_eq!(resolve_on_host(&global, None, 64).cpus.value.get(), 2);

    global.limits.max_cpus = Some(cpus(100));
    assert_eq!(resolve_on_host(&global, Some(&own), 8).cpus.value.get(), 12);
}

#[test]
fn this_computers_core_count_is_at_least_one() {
    assert!(host_cpu_count() >= 1);
}
