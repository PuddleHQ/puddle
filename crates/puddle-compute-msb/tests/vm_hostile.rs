// SPDX-License-Identifier: GPL-3.0-or-later
//! Hostile-guest cases at tier V that need msb's own files: a root process in the guest tries to
//! make the host attach a file of its choosing as its root disk, and to map device registers.
//! The cases that hold for any runtime (no network, only routed vsock ports) are in the contract
//! suite. Each test boots a real microVM, so they run only under the VM profile (`vm_` prefix).
mod support;

use std::io::Read as _;
use std::path::Path;
use std::time::Duration;

use puddle_compute::{ExecRequest, Runtime, Sandbox};
use support::{cleanup, sh, spec};

/// Where the root-disk journal sat in the guest before msb v0.7.12: msb shared the sandbox's
/// runtime folder with the guest, writable, at `/.msb`. msb now exports only a guest-state folder
/// there and keeps the journal next to it on the host side, as `<sandbox>/root-disk.json`.
const GUEST_JOURNAL: &str = "/.msb/root-disk.json";

/// A SHA-256 over a file's content: its length, then every 1 MiB piece that is not all zeros,
/// with its offset. Two files have the same one exactly when their bytes are the same. A root
/// disk is 4 GiB and mostly holes, and a debug build hashes at about 20 MB/s, so the zero
/// pieces (found by a plain comparison) are left out of the hash, not out of the comparison.
#[expect(
    clippy::unwrap_used,
    reason = "a file the test cannot read fails the test"
)]
fn fingerprint(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    const PIECE: usize = 1 << 20;
    let zeros = vec![0_u8; PIECE];
    let mut file = std::fs::File::open(path).unwrap();
    let mut hasher = Sha256::new();
    hasher.update(file.metadata().unwrap().len().to_le_bytes());
    let mut buf = vec![0_u8; PIECE];
    let mut offset = 0_u64;
    loop {
        let n = read_piece(&mut file, &mut buf);
        let Some(piece) = buf.get(..n).filter(|p| !p.is_empty()) else {
            break;
        };
        if zeros.get(..n) != Some(piece) {
            hasher.update(offset.to_le_bytes());
            hasher.update(piece);
        }
        offset += n as u64;
    }
    support::mount_attack::hex(&hasher.finalize())
}

/// Fills `buf` from `file` unless the file ends first, so pieces start at fixed offsets.
#[expect(
    clippy::unwrap_used,
    reason = "a file the test cannot read fails the test"
)]
fn read_piece(file: &mut std::fs::File, buf: &mut [u8]) -> usize {
    let mut filled = 0;
    while let Some(rest) = buf.get_mut(filled..).filter(|r| !r.is_empty()) {
        let n = file.read(rest).unwrap();
        if n == 0 {
            break;
        }
        filled += n;
    }
    filled
}

/// As root in the guest, writes a root-disk journal at [`GUEST_JOURNAL`] in msb's shape whose
/// launch base is the sandbox's own disk (`own`, what msb compares with the sandbox's config) and
/// whose only layer is `target`.
#[expect(clippy::expect_used, reason = "a failed exec fails the test")]
async fn forge_journal<S: Sandbox>(sb: &S, own: &Path, target: &Path) {
    let forged = serde_json::json!({
        "schema": "microsandbox.runtime-root-disk/1",
        "launch_base": own,
        "layers": [{ "path": target, "format": "raw" }],
    });
    let out = sb
        .exec(
            ExecRequest::new(
                "sh",
                [
                    "-c".to_owned(),
                    format!(r#"printf '%s' "$1" > {GUEST_JOURNAL}"#),
                    "sh".to_owned(),
                    forged.to_string(),
                ],
            )
            .as_user("root"),
        )
        .await
        .expect("exec");
    assert_eq!(
        out.status.code,
        0,
        "the guest could not write into msb's guest share: {}",
        out.stderr_text()
    );
}

/// Guest root writes a root-disk journal into msb's guest share at `/.msb` that names another
/// workspace's disk, and then a host file, as its writable root disk (what the old layout's
/// guest-writable journal allowed, upstream advisory GHSA-3f94-vm6x-3jv3). msb keeps its journal
/// out of that share, so the write changes nothing: after each stop the journal on the host is
/// the one msb wrote (or still absent), the next start goes through with the sandbox's own disk
/// (a file written before is still there), and both target files hash the same as before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_hostile_hg44_a_guest_written_root_disk_journal_changes_nothing_at_the_next_start() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let other = spec(&settings, "hg44-other");
    let other_name = other.name.clone();
    let s = spec(&settings, "hg44");
    let n = s.name.clone();
    let host_file = settings.home().join("hg44-host-file");
    std::fs::write(&host_file, b"a host file no guest may write\n").unwrap();

    let o = rt.create(other).await.expect("create the other workspace");
    o.stop().await.unwrap();
    let mut sb = rt.create(s).await.expect("create");
    let sandboxes = settings.home().join("sandboxes");
    let sandbox_dir = sandboxes.join(n.as_str());
    let own = sandbox_dir.join("upper.ext4");
    let other_disk = sandboxes.join(other_name.as_str()).join("upper.ext4");
    assert!(own.is_file(), "no disk at {}", own.display());
    assert!(other_disk.is_file(), "no disk at {}", other_disk.display());
    let host_journal = sandbox_dir.join("root-disk.json");
    let written = sh(&sb, "echo hg44-own-disk > /root/hg44-marker").await;
    assert_eq!(written.status.code, 0, "{}", written.stderr_text());
    let targets = [&other_disk, &host_file];
    let before = targets.map(|t| fingerprint(t));

    // Each round: (journal on the host after the stop, the start's error if any, the marker read
    // after the start).
    let mut rounds = Vec::new();
    for target in targets {
        let journal = std::fs::read(&host_journal).ok();
        forge_journal(&sb, &own, target).await;
        sb.stop().await.unwrap();
        let landed = std::fs::read(&host_journal).ok();
        match rt.start(&n).await {
            Ok(started) => {
                let marker = sh(&started, "cat /root/hg44-marker")
                    .await
                    .stdout_text()
                    .into_owned();
                rounds.push((target.clone(), journal == landed, None, marker));
                sb = started;
            }
            Err(e) => {
                rounds.push((target.clone(), journal == landed, Some(e), String::new()));
                break;
            }
        }
    }
    sb.stop().await.unwrap();
    let after = targets.map(|t| fingerprint(t));
    cleanup(&rt, &[&n, &other_name]).await;
    // The run's home is removed with the run; this only tidies early.
    let _ = std::fs::remove_file(&host_file);
    assert_eq!(rounds.len(), 2, "a start failed: {rounds:?}");
    for (target, unchanged, refused, marker) in rounds {
        assert!(
            unchanged,
            "the guest's write changed msb's journal on the host (target {})",
            target.display()
        );
        assert!(
            refused.is_none(),
            "the start after a guest-written journal naming {} failed: {refused:?}",
            target.display()
        );
        assert_eq!(
            marker.trim(),
            "hg44-own-disk",
            "the sandbox's own disk is not attached"
        );
    }
    assert_eq!(before, after, "a target file changed");
}

/// Finds the virtio-mmio register windows (in `/proc/iomem`, else the kernel command line's
/// `virtio_mmio.device=` entries), then does what guest root would do to reach a device's
/// registers past the guest kernel: open `/dev/mem` read-write (or a node it makes for char 1:1
/// when the kernel has none) and map each window read-write. Prints `slot <addr>` per window,
/// `devmem present|absent`, `node <path> open|refused: <why>` and, per window it tried,
/// `<addr> mapped <magic>` or `<addr> refused: <errno>`.
const DEVMEM_PROBE: &str = r#"
use strict; use warnings;
my %seen; my @slots;
if (open(my $io, "<", "/proc/iomem")) {
  while (<$io>) { push @slots, hex($1) if /^\s*([0-9a-f]+)-[0-9a-f]+ : .*virtio/i && !$seen{$1}++ }
}
if (!@slots && open(my $cl, "<", "/proc/cmdline")) {
  my $c = <$cl>;
  push @slots, hex($1) while $c =~ /virtio_mmio\.device=\w+@(0x[0-9a-f]+)/gi;
}
printf "slot 0x%x\n", $_ for @slots;
my $node = "/dev/mem";
my $made = 0;
if (-e $node) { print "devmem present\n" } else {
  print "devmem absent\n";
  $node = "/dev/puddle-probe-mem";
  unlink $node;
  system("mknod", $node, "c", "1", "1") == 0 or do { print "node $node refused: mknod failed\n"; exit 0 };
  $made = 1;
}
my $fd;
if (!open($fd, "+<", $node)) { print "node $node refused: $!\n"; unlink $node if $made; exit 0 }
print "node $node open\n";
for my $slot (@slots) {
  # mmap(NULL, 4096, PROT_READ|PROT_WRITE, MAP_SHARED, fd, slot): x86_64 syscall 9.
  my $addr = syscall(9, 0, 4096, 3, 1, fileno($fd), $slot);
  if ($addr == -1) { printf "0x%x refused: %s\n", $slot, $!; next }
  # A virtio-mmio window starts with its magic value register ("virt").
  printf "0x%x mapped 0x%08x\n", $slot, unpack("L", unpack("P4", pack("Q", $addr)));
  syscall(11, $addr, 4096);
}
unlink $node if $made;
"#;

/// Guest root cannot reach a device's MMIO registers through physical memory: the guest kernel
/// has no `/dev/mem`, and a node root makes for it (char 1:1) cannot be opened, so not one of the
/// virtio-mmio windows maps. With `/dev/mem`, root could map every window read-write and drive a
/// device (the memory balloon among them) past the guest kernel, leaving the device model's own
/// checks as the only line. The probe is proven by finding the windows it tries.
#[cfg_attr(
    windows,
    ignore = "the Windows firmware is built from a kernel configuration that still has /dev/mem"
)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_hostile_hg54_guest_root_cannot_map_device_registers_through_physical_memory() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let s = spec(&settings, "hg54");
    let n = s.name.clone();
    let sb = rt.create(s).await.expect("create");
    let out = sb
        .exec(
            ExecRequest::new("perl", ["-e", DEVMEM_PROBE])
                .as_user("root")
                .with_timeout(Duration::from_secs(60)),
        )
        .await
        .expect("devmem probe");
    sb.stop().await.unwrap();
    cleanup(&rt, &[&n]).await;
    let report = out.stdout_text();
    eprintln!("{report}{}", out.stderr_text());
    assert_eq!(out.status.code, 0, "{}", out.stderr_text());
    let slots = report.lines().filter(|l| l.starts_with("slot ")).count();
    assert!(slots > 0, "the probe found no virtio-mmio window: {report}");
    let mapped = report.lines().filter(|l| l.contains(" mapped ")).count();
    assert!(
        report.lines().any(|l| l == "devmem absent"),
        "the guest has /dev/mem: {report}"
    );
    assert!(
        report
            .lines()
            .any(|l| l.starts_with("node ") && l.contains(" refused: ")),
        "root opened a physical-memory node: {report}"
    );
    assert_eq!(
        mapped, 0,
        "{mapped} of {slots} device windows mapped: {report}"
    );
}
