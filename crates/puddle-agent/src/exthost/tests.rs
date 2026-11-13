// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn p(id: i32, parent: i32, argv: &[&str]) -> Proc {
    Proc {
        pid: id,
        ppid: parent,
        start: u64::try_from(id).unwrap() * 10,
        zombie: false,
        argv: argv.iter().map(|a| (*a).to_owned()).collect(),
        inspector: None,
    }
}

fn servers() -> Vec<PathBuf> {
    vec![PathBuf::from("/puddle/code-server")]
}

/// code-server's own children, as `ps` shows them: a pty host, an extension host with a
/// language server and a debuggee below it.
fn table() -> Vec<Proc> {
    vec![
        p(1, 0, &["/sbin/init"]),
        p(
            10,
            1,
            &[
                "/puddle/code-server/lib/node",
                "/puddle/code-server/out/node/entry",
            ],
        ),
        p(
            20,
            10,
            &[
                "/puddle/code-server/lib/node",
                "/puddle/code-server/lib/vscode/out/bootstrap-fork",
                "--type=extensionHost",
            ],
        ),
        p(21, 20, &["node", "tsserver.js"]),
        p(22, 21, &["node", "app.js"]),
        p(
            30,
            10,
            &[
                "/puddle/code-server/lib/node",
                "/puddle/code-server/lib/vscode/out/bootstrap-fork",
                "--type=ptyHost",
            ],
        ),
        p(31, 30, &["bash"]),
        p(32, 31, &["sleep", "600"]),
    ]
}

#[test]
fn an_extension_host_is_found_by_its_argument_and_the_servers_folder() {
    assert_eq!(extension_hosts(&table(), &servers()), [20]);
}

#[test]
fn a_host_of_something_else_or_a_dead_one_is_not_one() {
    let mut procs = table();
    // The same argument in a program outside the servers' folders (a user's own Electron app).
    procs.push(p(40, 1, &["/opt/app/electron", "--type=extensionHost"]));
    // A folder that only shares a prefix with the server's.
    procs.push(p(
        41,
        1,
        &["/puddle/code-server-old/lib/node", "--type=extensionHost"],
    ));
    let mut zombie = p(
        42,
        1,
        &["/puddle/code-server/lib/node", "--type=extensionHost"],
    );
    zombie.zombie = true;
    procs.push(zombie);
    assert_eq!(extension_hosts(&procs, &servers()), [20]);
    assert_eq!(extension_hosts(&procs, &[]), Vec::<i32>::new());
}

#[test]
fn a_child_the_host_forked_that_has_not_run_its_program_yet_is_not_another_host() {
    let mut procs = table();
    // Between `fork` and `exec` a child shows its parent's command line.
    let host = procs.iter().find(|p| p.pid == 20).unwrap().clone();
    procs.push(Proc {
        pid: 23,
        ppid: 20,
        start: 230,
        ..host
    });
    assert_eq!(extension_hosts(&procs, &servers()), [20]);
    assert!(tree(&procs, &[20]).contains(&23));
}

#[test]
fn the_tree_is_the_host_and_everything_below_it_and_not_the_terminals() {
    let found = tree(&table(), &[20]);
    assert_eq!(found, HashSet::from([20, 21, 22]));
    assert!(!found.contains(&30) && !found.contains(&32));
}

#[test]
fn a_child_of_init_with_a_dead_debug_socket_is_an_orphan_and_nothing_else_is() {
    let opts = |sock: &str| {
        Some(format!(
            "{{\"inspectorIpc\":\"{sock}\",\"deferredMode\":false}}"
        ))
    };
    let mut gone = p(50, 1, &["node", "dbg.js"]);
    gone.inspector = opts("/tmp/node-cdp.20-1.sock");
    let mut alive = p(51, 1, &["node", "dbg.js"]);
    alive.inspector = opts("/tmp/node-cdp.99-1.sock");
    let mut no_path = p(52, 1, &["node", "dbg.js"]);
    no_path.inspector = Some("{}".into());
    let mut deferred = p(56, 1, &["node", "job.js"]);
    deferred.inspector = Some("{\"inspectorIpc\":\"/tmp/gone.sock\",\"deferredMode\":true}".into());
    let mut unreadable = p(53, 1, &["node", "dbg.js"]);
    unreadable.inspector = Some("not json".into());
    // Not a child of init: its parent can still use it.
    let mut parented = p(54, 33, &["node", "dbg.js"]);
    parented.inspector = opts("/tmp/node-cdp.20-1.sock");
    // A job of the user's: no inspector options.
    let job = p(55, 1, &["sleep", "600"]);
    let procs = [gone, alive, no_path, unreadable, parented, job, deferred];
    // flake-ok: a value that is parsed or compared and never opened, so the missing drive letter does not matter
    let exists = |path: &Path| path == Path::new("/tmp/node-cdp.99-1.sock");
    assert_eq!(orphan_debuggees(&procs, 1, exists), [50]);
}

#[test]
fn only_the_named_reapers_children_are_orphans() {
    let mut debuggee = p(60, 7, &["node", "dbg.js"]);
    debuggee.inspector = Some("{\"inspectorIpc\":\"/gone\",\"deferredMode\":false}".into());
    assert_eq!(orphan_debuggees(&[debuggee.clone()], 7, |_| false), [60]);
    assert_eq!(
        orphan_debuggees(&[debuggee], 1, |_| false),
        Vec::<i32>::new()
    );
}

#[cfg(target_os = "linux")]
#[tokio::test(start_paused = true)]
async fn waiting_for_the_killed_to_leave_gives_up_after_its_polls() {
    let me = i32::try_from(std::process::id()).unwrap();
    let start = snapshot().iter().find(|p| p.pid == me).unwrap().start;
    let started = tokio::time::Instant::now();

    // This process never leaves the table.
    kill_settle(&HashMap::from([(me, start)])).await;

    assert_eq!(started.elapsed(), POLL * SETTLE_POLLS);
}

#[cfg(target_os = "linux")]
#[test]
fn stat_is_read_with_a_name_that_holds_spaces_and_parentheses() {
    let stat = "123 (a) b (c) S 77 1 1 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 555 1000 10 18446744073709551615";
    assert_eq!(parse_stat(stat), Some((false, 77, 555)));
    assert_eq!(
        parse_stat("5 (x) Z 6 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 9 0 0"),
        Some((true, 6, 9))
    );
    assert_eq!(parse_stat("garbage"), None);
    assert_eq!(parse_stat("1 (x) S 2"), None);
}
