// SPDX-License-Identifier: GPL-3.0-or-later
//! Root's write attack on a file Puddle mounts into a guest, and the judge of what came of it.
//!
//! Three parts, so that the test which uses them can be shown to fail:
//!
//! - [`ATTACK`]: one perl script that tries every way to change a file or put something beside it
//!   and prints the errno each one got, as `<op> <outcome>` lines.
//! - [`expected`]: the one outcome each operation must have, per state of the guest's mount flag,
//!   with the reason (the kernel's order of checks).
//! - [`HostDir`] and [`judge`]: the host's view. The guest's error text proves only that the guest
//!   said no; the host's own files (hash, length, modification and change time, mode, owner, links,
//!   the folder's entries) before and after prove that nothing arrived. `judge` fails on any
//!   unexpected outcome and on any host difference.
//!
//! The control tests (`mount_attack_control.rs`) feed `judge` what a writable share would produce
//! and require it to fail, so a judge that passed everything would be caught.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers: a failed step fails the test; each test binary uses a different subset"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::SystemTime;

use sha2::{Digest, Sha256};

/// Every operation [`ATTACK`] tries, in the order it tries them: the ones that would destroy the
/// file come last, so that on a writable share (the negative control) the others still ran on it.
pub(crate) const OPS: [&str; 15] = [
    "read",
    "open_write",
    "append",
    "o_trunc",
    "truncate",
    "chmod",
    "chown",
    "utimes",
    "link",
    "mknod_over",
    "rename_over",
    "create_beside",
    "mknod_beside",
    "rename_away",
    "unlink",
];

/// The attack. Arguments: the file. It prints `state ro|rw|none` (the mount flag of exactly that
/// file in `/proc/mounts`), `running yes|no` (whether a process executes it), then `<op> <outcome>`
/// for every name in [`OPS`]; an outcome is `ok` or the errno's name (`errnoN` when unnamed).
///
/// The writes go through `sysopen`, `syswrite`, `fsync` and `close`, and the first one that fails
/// names the outcome, because a write-back cache may only meet the host's refusal at the flush.
/// `mknod` is coreutils' (perl has no wrapper), so its errno comes back as the C library's message
/// and is mapped to the name by the same table.
pub(crate) const ATTACK: &str = r#"
use strict; use warnings; use Fcntl; use IO::Handle;
my ($f) = @ARGV;
my %name = (1 => 'EPERM', 2 => 'ENOENT', 13 => 'EACCES', 16 => 'EBUSY', 17 => 'EEXIST', 18 => 'EXDEV',
            22 => 'EINVAL', 26 => 'ETXTBSY', 30 => 'EROFS');
my %by_text = (
  'Operation not permitted' => 'EPERM', 'No such file or directory' => 'ENOENT',
  'Permission denied' => 'EACCES', 'Device or resource busy' => 'EBUSY', 'File exists' => 'EEXIST',
  'Invalid cross-device link' => 'EXDEV', 'Invalid argument' => 'EINVAL',
  'Text file busy' => 'ETXTBSY', 'Read-only file system' => 'EROFS',
);
sub failed { my $n = $! + 0; return $name{$n} // "errno$n"; }
sub out { my ($op, $outcome) = @_; print "$op $outcome\n"; }
sub attempt { my ($op, $code) = @_; out($op, $code->() ? 'ok' : failed()); }

my $state = 'none';
if (open(my $m, '<', '/proc/mounts')) {
  while (my $l = <$m>) {
    my (undef, $at, undef, $opts) = split ' ', $l;
    next unless defined $opts && $at eq $f;
    $state = (grep { $_ eq 'rw' } split /,/, $opts) ? 'rw' : 'ro';
  }
}
print "state $state\n";
my $running = 'no';
for my $exe (glob '/proc/[0-9]*/exe') {
  my $to = readlink $exe;
  if (defined $to && $to eq $f) { $running = 'yes'; last; }
}
print "running $running\n";

# Opens `$f` with `$flags`, writes a byte, flushes and closes; the first step that fails is the outcome.
sub write_through {
  my ($flags) = @_;
  sysopen(my $h, $f, $flags) or return 0;
  my $ok = defined(syswrite($h, 'x')) && $h->sync;
  my $err = $!;
  my $closed = close($h);
  $! = $err unless $ok;
  return $ok && $closed;
}
sub mknod_outcome {
  my ($path) = @_;
  my $text = `mknod -m 600 '$path' p 2>&1`;
  return 'ok' if $? == 0;
  $text =~ s/.*: //s; $text =~ s/\s+$//;
  return $by_text{$text} // "text($text)";
}

attempt('read', sub { open(my $h, '<', $f) or return 0; return defined(sysread($h, my $buf, 16)); });
attempt('open_write', sub { write_through(O_WRONLY) });
attempt('append', sub { write_through(O_WRONLY | O_APPEND) });
attempt('o_trunc', sub { write_through(O_WRONLY | O_TRUNC) });
attempt('truncate', sub { truncate($f, 0) });
attempt('chmod', sub { chmod(0777, $f) == 1 });
attempt('chown', sub { chown(12345, 12345, $f) == 1 });
attempt('utimes', sub { utime(1, 1, $f) == 1 });
attempt('link', sub { link($f, "$f.link") });
out('mknod_over', mknod_outcome($f));
{
  my $src = "$f.src";
  sysopen(my $s, $src, O_WRONLY | O_CREAT | O_EXCL, 0600) or die "cannot create $src: $!";
  close $s;
  attempt('rename_over', sub { rename($src, $f) });
  unlink $src;
}
attempt('create_beside', sub { sysopen(my $h, "$f.new", O_WRONLY | O_CREAT | O_EXCL, 0600) });
out('mknod_beside', mknod_outcome("$f.node"));
attempt('rename_away', sub { rename($f, "$f.moved") });
attempt('unlink', sub { unlink($f) == 1 });
# Leave nothing of the guest's own beside the file (only matters where those succeeded).
unlink "$f.link", "$f.new", "$f.node", "$f.moved";
"#;

/// What [`ATTACK`] printed for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Report {
    /// The mount flag the script found for the file (`ro`, `rw` or `none`).
    pub(crate) state: String,
    /// Whether a process executed the file while the attack ran.
    pub(crate) running: bool,
    /// Each operation's outcome.
    pub(crate) outcomes: BTreeMap<String, String>,
}

impl Report {
    /// Reads the script's output; panics on a line it cannot place, so a script that broke is a
    /// failed test and not a quiet pass.
    pub(crate) fn parse(text: &str) -> Self {
        let (mut state, mut running) = (String::new(), false);
        let mut outcomes = BTreeMap::new();
        for line in text.lines() {
            let (key, value) = line.split_once(' ').expect(line);
            match key {
                "state" => value.clone_into(&mut state),
                "running" => running = value == "yes",
                op => {
                    assert!(OPS.contains(&op), "unknown operation in the report: {line}");
                    outcomes.insert(op.to_owned(), value.to_owned());
                }
            }
        }
        Self {
            state,
            running,
            outcomes,
        }
    }

    /// A report built from outcomes, for the control tests.
    pub(crate) fn of(state: &str, running: bool, outcomes: &[(&str, &str)]) -> Self {
        Self {
            state: state.to_owned(),
            running,
            outcomes: outcomes
                .iter()
                .map(|(op, o)| ((*op).to_owned(), (*o).to_owned()))
                .collect(),
        }
    }
}

/// The outcome `op` must have on a boot mount whose guest-side flag is read-only (`read_only`) or
/// cleared.
///
/// The guest's kernel answers before the host is asked, in this order, so the errno says which
/// layer refused:
///
/// - Flag `ro`: the mount's read-only flag is checked first (`inode_permission`,
///   `mnt_want_write`), so every change to the file is `EROFS`, also on a running binary.
/// - Flag `rw` (root cleared it with `mount -o remount,rw`; the host's share is still read-only):
///   - opening for write, appending and truncating a *running binary* is `ETXTBSY`: the kernel's
///     deny-write on an executing file comes before the file system is asked. It says nothing about
///     the host, which is why the attribute changes below (never blocked by a running binary) and
///     the host-side check carry the proof for the agent. On a file nothing executes, the same
///     operations reach the host and its share answers `EROFS`.
///   - `chmod`, `chown` and `utimes` always reach the host: `EROFS`.
/// - In both: `link` is `EXDEV` (the file is another mount than its folder); the file is a mount
///   point, so removing it or renaming it away or over it is `EBUSY` and the host is never asked;
///   `mknod` over it is `EEXIST` (the name is taken, checked before the flag).
/// - Beside the file, in the guest's own folder, a new name is the guest's own business (`ok`): it
///   is not on the share, and the host-side check shows that no new name reached the host's folder.
pub(crate) fn expected(read_only: bool, op: &str, running: bool) -> &'static str {
    match op {
        "read" | "create_beside" | "mknod_beside" => "ok",
        "open_write" | "append" | "o_trunc" | "truncate" if running && !read_only => "ETXTBSY",
        "open_write" | "append" | "o_trunc" | "truncate" | "chmod" | "chown" | "utimes" => "EROFS",
        "link" => "EXDEV",
        "mknod_over" => "EEXIST",
        "rename_over" | "rename_away" | "unlink" => "EBUSY",
        _ => panic!("no expectation for {op}"),
    }
}

/// What the host can tell about one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostFile {
    pub(crate) sha256: String,
    pub(crate) len: u64,
    pub(crate) modified: SystemTime,
    /// Mode, owner, group, inode, link count and change time (what chmod, chown, link and any
    /// other inode change touch).
    #[cfg(unix)]
    pub(crate) inode: (u32, u32, u32, u64, u64, i64, i64),
    /// The Windows attribute bits (the read-only bit among them).
    #[cfg(windows)]
    pub(crate) attributes: u32,
}

impl HostFile {
    /// The file's state, or `None` when it is not there.
    fn read(path: &Path) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            sha256: sha256_hex(&bytes),
            len: meta.len(),
            modified: meta.modified().unwrap(),
            #[cfg(unix)]
            inode: {
                use std::os::unix::fs::MetadataExt;
                (
                    meta.mode(),
                    meta.uid(),
                    meta.gid(),
                    meta.ino(),
                    meta.nlink(),
                    meta.ctime(),
                    meta.ctime_nsec(),
                )
            },
            #[cfg(windows)]
            attributes: {
                use std::os::windows::fs::MetadataExt;
                meta.file_attributes()
            },
        })
    }
}

/// The SHA-256 of `bytes` in lower-case hex.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Lower-case hex of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The host's folder that holds the mounted files: each named file's state and the names in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostDir {
    pub(crate) files: BTreeMap<String, Option<HostFile>>,
    pub(crate) entries: BTreeSet<String>,
}

impl HostDir {
    /// Reads `names` in `dir` and lists every entry of `dir`.
    pub(crate) fn take(dir: &Path, names: &[&str]) -> Self {
        Self {
            files: names
                .iter()
                .map(|n| ((*n).to_owned(), HostFile::read(&dir.join(n))))
                .collect(),
            entries: std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect(),
        }
    }

    /// Each way `after` differs from `self`, named by what changed.
    pub(crate) fn differences(&self, after: &Self) -> Vec<String> {
        let mut found = Vec::new();
        for (name, was) in &self.files {
            let (Some(was), Some(now)) = (was, after.files.get(name).and_then(Option::as_ref))
            else {
                found.push(format!("{name}: gone"));
                continue;
            };
            let mut field = |what: &str, changed: bool| {
                if changed {
                    found.push(format!("{name}: {what} changed"));
                }
            };
            field("sha256", was.sha256 != now.sha256);
            field("length", was.len != now.len);
            field("modification time", was.modified != now.modified);
            #[cfg(unix)]
            {
                let (a, b) = (was.inode, now.inode);
                field("mode", a.0 != b.0);
                field("owner", (a.1, a.2) != (b.1, b.2));
                field("inode", a.3 != b.3);
                field("link count", a.4 != b.4);
                field("change time", (a.5, a.6) != (b.5, b.6));
            }
            #[cfg(windows)]
            field("attributes", was.attributes != now.attributes);
        }
        for name in self.entries.symmetric_difference(&after.entries) {
            let verb = if after.entries.contains(name) {
                "appeared"
            } else {
                "vanished"
            };
            found.push(format!("{name}: {verb} in the host folder"));
        }
        found
    }
}

/// Every reason this attack on `target` is not the refusal the mounts promise: an outcome that is
/// not [`expected`], a missing one, and a flag that is neither `ro` nor `rw`.
pub(crate) fn judge_outcomes(target: &str, report: &Report) -> Vec<String> {
    let mut failures = Vec::new();
    if report.state != "ro" && report.state != "rw" {
        failures.push(format!(
            "{target}: the mount flag is {:?}, not a mount",
            report.state
        ));
    }
    for op in OPS {
        match report.outcomes.get(op) {
            None => failures.push(format!("{target}: {op} has no outcome")),
            Some(got) => {
                let want = expected(report.state == "ro", op, report.running);
                if got != want {
                    failures.push(format!(
                        "{target}: {op} gave {got}, must give {want} (flag {}, running {})",
                        report.state, report.running
                    ));
                }
            }
        }
    }
    failures
}

/// [`judge_outcomes`] plus each difference between the host's files `before` and `after` the
/// attack: the whole judgement of one attack.
pub(crate) fn judge(
    target: &str,
    report: &Report,
    before: &HostDir,
    after: &HostDir,
) -> Vec<String> {
    let mut failures = judge_outcomes(target, report);
    failures.extend(
        before
            .differences(after)
            .into_iter()
            .map(|d| format!("{target}: on the host, {d}")),
    );
    failures
}
