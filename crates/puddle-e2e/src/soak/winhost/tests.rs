// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn proc(pid: u32, parent: u32, exe: &str) -> Proc {
    Proc {
        pid,
        parent,
        exe: exe.to_owned(),
        threads: 4,
    }
}

fn machine() -> Vec<Proc> {
    vec![
        proc(4, 0, "System"),
        proc(100, 4, "Puddle-App.EXE"),
        proc(101, 100, "puddle.exe"),
        proc(102, 101, "msb.exe"),
        proc(103, 101, "msb.exe"),
        proc(200, 4, "puddle.exe"),
        proc(201, 200, "msb.exe"),
        proc(300, 4, "puddle-soak.exe"),
    ]
}

fn names(found: &[(String, u32)]) -> Vec<String> {
    found.iter().map(|(c, p)| format!("{c}.{p}")).collect()
}

#[test]
fn a_process_is_named_by_its_executable_without_the_extension_in_lower_case() {
    assert_eq!(comm("Puddle-App.EXE"), "puddle-app");
    assert_eq!(comm("msb.exe"), "msb");
    assert_eq!(comm("System"), "system");
}

#[test]
fn processes_are_found_by_name_prefix_and_a_dollar_makes_it_a_whole_name() {
    let prefixes = vec![
        "puddle-app".to_owned(),
        "puddle$".to_owned(),
        "msb".to_owned(),
    ];
    let found = select(&machine(), &prefixes, None);
    // `puddle-soak` is neither a prefix hit nor the whole name `puddle`.
    assert_eq!(
        names(&found),
        [
            "msb.102",
            "msb.103",
            "msb.201",
            "puddle.101",
            "puddle.200",
            "puddle-app.100"
        ]
    );
}

#[test]
fn only_the_tree_below_the_runs_own_root_is_measured() {
    let prefixes = vec!["puddle$".to_owned(), "msb".to_owned()];
    let found = select(&machine(), &prefixes, Some(100));
    // The second Puddle (200) and its msb (201) belong to another run of the machine.
    assert_eq!(names(&found), ["msb.102", "msb.103", "puddle.101"]);
}

#[test]
fn a_parent_id_cycle_does_not_hang_the_tree_walk() {
    let all = vec![proc(1, 2, "puddle.exe"), proc(2, 1, "puddle.exe")];
    let found = select(&all, &["puddle$".to_owned()], Some(1));
    assert_eq!(names(&found), ["puddle.1", "puddle.2"]);
}

#[test]
fn the_memory_readings_are_kib_and_the_commit_charge_is_limit_minus_available() {
    let gib = 1024_u64 * 1024 * 1024;
    let mem = Memory {
        avail_phys: 5 * gib,
        commit_limit: 20 * gib,
        commit_avail: 12 * gib,
    };
    let r = mem.readings();
    let get = |name: &str| r.iter().find(|(n, _)| *n == name).map(|&(_, v)| v);
    assert_eq!(get("free_ram_kb"), Some(5.0 * 1024.0 * 1024.0));
    assert_eq!(get("commit_kb"), Some(8.0 * 1024.0 * 1024.0));
    assert_eq!(get("commit_limit_kb"), Some(20.0 * 1024.0 * 1024.0));
}
