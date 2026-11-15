// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn byte_counts_parse_from_util_linux_and_busybox() {
    assert_eq!(
        trimmed_bytes("/workspaces/a: 1 GiB (1073741824 bytes) trimmed\n"),
        Some(1_073_741_824)
    );
    assert_eq!(
        trimmed_bytes("/workspaces/a: 4096 bytes trimmed\n"),
        Some(4096)
    );
    assert_eq!(
        trimmed_bytes("/workspaces/a: 0 B (0 bytes) trimmed"),
        Some(0)
    );
    assert_eq!(trimmed_bytes(""), None);
    assert_eq!(trimmed_bytes("something else"), None);
    assert_eq!(trimmed_bytes("/w: lots (x bytes) trimmed"), None);
}

#[test]
fn the_request_trims_the_mount_point_as_root() {
    let l = Layout::new(&WorkspaceId::new("a").unwrap()).unwrap();
    let r = request(&l);
    assert_eq!(r.program, "fstrim");
    assert_eq!(r.args, ["-v", "/workspaces/a"]);
    assert_eq!(r.user.as_deref(), Some("root"));
    assert_eq!(r.timeout, TRIM_TIMEOUT);
}

#[test]
fn tails_keep_the_end() {
    assert_eq!(tail("  short \n"), "short");
    let long = format!("{}end", "x".repeat(1000));
    let t = tail(&long);
    assert_eq!(t.chars().count(), STDERR_TAIL);
    assert!(t.ends_with("end"));
}
