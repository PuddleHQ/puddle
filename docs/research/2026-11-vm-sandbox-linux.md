# Sandboxing the VM process on Linux

Date: 2026-11-12
Verdict: **DRAWBACKS** (details at the end). A seccomp allowlist of 105 system calls plus `no_new_privs` and a
Landlock ruleset ran the whole Linux VM test suite on a kernel with Landlock, and the seccomp part ran it on a
kernel without. But Landlock is missing or partial on kernels Puddle supports, a Landlock denial reads as a plain
"Permission denied", the ruleset that passes still lets the VMM list every directory and connect to any Unix
socket the user can, and every workspace shares one writable msb home. A wrapper around the runtime binary also
breaks what inspects that binary.

This is research. No product code changed.

## The problem

On Linux, a workspace's VM runs in `msb machine`, the msb runtime process that hosts libkrun's VMM. The msb SDK
inside Puddle starts it with the user's full rights. Its only isolation is a new session (`setsid`). There is no
seccomp filter, no `no_new_privs`, no Landlock and no namespace. libkrun carries a `SIGSYS` handler for seccomp
faults but never registers it, and installs no filter. A guest that finds one bug in a device the VMM emulates
(the virtio-fs server, block through imago, balloon, console, vsock, rng) runs code as the user, with the user's
files, sockets and credentials in reach. libkrun's own security model says the virtio-fs export is no sandbox for
the VMM and recommends confining it with the host's own controls.

## How this was measured

- **Debian 13 host with KVM** (Proxmox kernel `7.0.14-6-pve`, an LXC container, 40 cores, 128 GB, glibc 2.41).
  Landlock is compiled in but off at boot there: `landlock_create_ruleset(..., LANDLOCK_CREATE_RULESET_VERSION)`
  returns `EOPNOTSUPP`. The host runs every other check of the project, so its load moved between 14 and 66
  during the runs.
- **GitHub's `ubuntu-24.04` runner** (kernel `7.0.0-1017-azure`, Landlock ABI 8, glibc 2.39). Runs 43124052972,
  43125575850, 43128284029, 43130416957, 43132444937, 43134451798, 43138168063, 43141914756, 43145505130 and 43156687358 of the
  `vm-linux` workflow on a research branch that never lands (a step after the runtime download installs the
  prototype). Baseline: the scheduled develop run 43006488632, 168 of 168 passed.
- **WSL2** (kernel `6.6.114.1-microsoft-standard-WSL2`, Landlock ABI 3, no `/dev/kvm`): Landlock behaviour and
  path-walk cost only.
- The msb runtime is the pinned fork release `v0.7.12-puddle.3` (libkrun fork tag `msb_krun-v0.1.42-puddle.1`),
  the glibc build the releases ship (dynamically linked, needs glibc 2.34 or later).
- The VM test suite is the repository's own (`vm-linux`'s selection): 176 tests on the host (the CI selection plus
  the soak tests), 168 on CI.
- **Prototype** (throwaway, outside the product): `msb-jail`, a small C program installed in place of the `msb`
  binary, with the real binary renamed beside it. It carries a copy of the real binary's `.msbver` section,
  because the SDK reads the runtime version from that section and treats a script (`#!`) as an older runtime with
  another launch contract. Per mode it:
  - `strace`: runs the runtime under `strace -D -f -ff -tt -y` (the `-D` form keeps the runtime's process id, which
    the SDK checks);
  - `jail`: sets `no_new_privs`, applies a Landlock ruleset, installs a seccomp allowlist, then `exec`s the real
    runtime. A syscall outside the allowlist either goes to a supervisor process through
    `SECCOMP_RET_USER_NOTIF`, which logs thread, syscall and arguments and then lets it run (log mode) or fails it
    with `EPERM` (deny mode), or kills the process (`SECCOMP_RET_KILL_PROCESS`). The supervisor gets the listener
    with `pidfd_getfd` through a shared page, so the wrapper makes no filtered call before the supervisor holds it.
  - `SECCOMP_RET_LOG` was not usable: its records go to the kernel audit log, which an unprivileged user (and an
    LXC container) cannot read. The user-notification supervisor is the log mode that works without root.

## 1. What the VM process needs

Measured with the whole suite under `strace` on the Debian host: 172 of 176 tests passed (the four failures are
strace's cost and its own exit code, listed under section 7). 1,454 runtime launches, 9,233 threads.

**Processes.** The SDK starts one `msb machine --name … --config-fd 96 --lifecycle-lock-fd 99 --parent-watch-fd 97`
per sandbox, and probes the binary first with short `msb __launch-protocol` runs (four to nine per start). The
`machine` process starts no other process. The msb **CLI** does (`msb run` forks and `setsid`s the VMM): Puddle's
doctor test boot uses it.

**System calls.** 107 distinct calls over the suite, 105 after removing three that only strace's own attach
attempts made (`ptrace`, `alarm`, `pause`) and adding `listxattr`, which only a CI run made (below). Per thread role (the
name each thread gives itself):

| Thread | Calls | What it does that matters |
|---|---|---|
| main | 80 | creates the VM (`/dev/kvm`), sockets (`agent.sock`, `control.sock`), SQLite, the trampoline root, `memfd_create` for the init binary, `pidfd_open` |
| vCPU (`fc_vcpu N`) | 23 | `ioctl(KVM_RUN)` (4.9 M calls), `clock_gettime`, `write` to eventfds, `sched_yield`, `futex` |
| virtio-fs (`fs worker`) | 37 | `openat2` (`RESOLVE_BENEATH`), `statx`, `getxattr`/`setxattr`, `renameat2`, `unlinkat`, `mkdirat`, `utimensat`, `preadv`/`pwritev` |
| block (`block worker`, `virtio-blk-writer`) | 27 + 14 | `preadv`, `pwritev`, `fdatasync`, `fallocate`, `ftruncate`, `sync_file_range` |
| vsock (`vsock muxer`, `vsock reaper`) | 19 + 12 | `recvfrom`, `sendto`, `epoll_*` on the host Unix sockets of the routes |
| console (`console port`) | 18 | `read`, `write`, `poll` |
| VMM event loop (`vmm worker`) | 16 | `ioctl` on the VM fd (interrupt routing), `sched_yield` |
| control, log, parent watch, SQLite, tokio workers | 12 to 65 | `accept4`, `recvmsg`, `rename`, `fsync`, `clock_nanosleep`, `mremap` |

Not used anywhere: `io_uring_*`, `userfaultfd`, `bpf`, `perf_event_open`, `ptrace`, `mount`, `unshare`, `setns`,
`keyctl`, `add_key`, `process_vm_*`, `execveat`, `kexec_*`, `init_module`, `chroot`, `pivot_root`. `execve` appears
only once per launch, the wrapper's own exec of the runtime. The full list is the appendix.

**KVM ioctls** (22 distinct): on `/dev/kvm` `GET_API_VERSION`, `CREATE_VM`, `GET_MSR_INDEX_LIST`, `CHECK_EXTENSION`,
`GET_VCPU_MMAP_SIZE`, `GET_SUPPORTED_CPUID`; on the VM `CREATE_VCPU`, `SET_USER_MEMORY_REGION`, `SET_TSS_ADDR`,
`ENABLE_CAP`, `CHECK_EXTENSION`, `IRQFD`, `IOEVENTFD`, `SET_GSI_ROUTING` (158,000 calls: MSI routing updates);
on each vCPU `RUN`, `SET_REGS`, `GET_SREGS`, `SET_SREGS`, `SET_FPU`, `GET_LAPIC`, `SET_LAPIC`, `SET_MSRS`,
`SET_CPUID2`. Other ioctls: `TCGETS` on `/dev/null` and `FIONBIO`.

**Files.** The msb home (database, image cache, sandbox folders, `run/` sockets and lock files), the guest share
and every read-only mount source, a checkout's `.git` outside Puddle's folders (the one mount source that may lie
anywhere), `/dev/kvm`, `/dev/null`, `/dev/urandom`, `/dev/shm/msb-met-*` (a metrics segment shared with the SDK),
the runtime folder (`libkrunfw.so.5`), `/etc/ld.so.cache`, `/etc/resolv.conf`, `/etc/microsandbox/managed.json`
(msb's administrator config), the system libraries, and in `/proc` and `/sys`: `self/fd`, `self/maps`,
`self/mountinfo`, `self/cgroup`, `meminfo`, `sys/kernel/random/boot_id`, `sys/kernel/pid_max`,
`sys/vm/dirty_background_bytes`, `sys/vm/overcommit_memory`, `devices/system/cpu/online`, the cgroup `cpu.max` files.
The passthrough file server opens `/` itself as a directory and resolves every share path beneath it with
`openat2(RESOLVE_BENEATH)`, then reopens files through `/proc/self/fd/N`.

**Sockets.** Unix stream sockets it binds (`agent.sock`, `control.sock`) and the host sockets of the vsock routes
it connects to. Two UDP sockets `connect` to `192.0.2.1:443` and `2001:db8::1:443` (a route probe; nothing is
sent). Two ICMP datagram sockets fail with `EACCES` (the host's `ping_group_range`). These four come only from tests that
turn on msb's own user-mode network, which Puddle keeps off. No TCP connect or bind. `memfd_create` (the guest
init binary) is the only anonymous file.

**Features covered.** Live balloon and free-page reporting, root-disk grow live and offline, trim, snapshots,
restore of an unheld volume, capture and the DNS stub, git over HTTPS and SSH, SSH attach and the desktop attach,
the browser editors, extra shares and checkouts, port forwards across a restart, nested Docker and Docker-in-Docker,
the kind warning, 2,000 concurrent guest connections, 256 parallel vsock downloads, flood caps, the doctor and the
soak tests (15 minutes idle SSH, 5 minutes host pause).

## 2. Seccomp

- **Who could install it.** libkrun has no filter of its own; its `SIGSYS` handler (inherited from Firecracker) is
  never registered. msb installs none. So it is ours to add: a launcher that applies it before `exec` (the
  prototype), a patch in the msb fork at the start of `msb machine`, or a patch in the libkrun fork per thread.
  Prior art: BoxLite applies a Firecracker-style filter around libkrun and reports that its entries had to be
  widened to whole syscalls to make libkrun work.
- **Size.** 105 syscalls for the runtime; the filter is 217 BPF instructions with a linear compare. The kernel's
  seccomp cache (Linux 5.11 and later) answers a syscall that the filter always allows without running the
  program, so the hot calls (`ioctl`, `futex`, `read`, `write`) cost nothing extra.
- **Per thread.** Possible only inside the process: a filter is inherited at `clone` and can only be narrowed. A
  vCPU thread needs 23 calls, so a second filter at thread start (libkrun fork) would shrink what a vCPU-side bug
  reaches. A launcher can only give every thread the union.
- **Arguments.** `ioctl` can be narrowed to the 22 KVM requests plus `TCGETS`/`FIONBIO` by request number (a
  launcher can do that; it was not tried). `socket` can be narrowed by family. `clone3` cannot be: its flags are
  in memory, which seccomp cannot read. The usual answer is to fail `clone3` with `ENOSYS` (glibc then uses
  `clone`, whose flags can be checked, so `CLONE_NEWUSER` can be refused).
- **What `exec` costs.** A filter applied before `exec` must allow `execve`. Inside msb the runtime could add a
  second filter after start that refuses `execve`, `clone` with `CLONE_NEW*` and the socket families it never uses.
- **Variance.** The runtime is linked against the host's glibc, so its syscalls follow the user's distribution:
  `clone3` (glibc 2.34), `rseq` (2.35), `fchmodat2` (2.39), the vDSO `getrandom` (2.41) are examples of calls a
  newer glibc starts to use. Measured: on the Ubuntu runner (glibc 2.39) the suite made no call the Debian 13 host
  (glibc 2.41) had not made, except one: `listxattr`, from the virtio-fs thread, because a guest listed
  extended attributes in that run. So the set also depends on what the guest does: the file server turns guest
  requests into host calls, and a request the suite never sends (an attribute list, a copy range, a lock) can
  need a call no trace saw. The allowlist has to come from the file server's request handlers, not only from
  traces. Ubuntu 22.04 (glibc 2.35, kernel 5.15),
  Fedora, Arch and a musl build were not measured. A static musl runtime would fix the set at build time, but msb
  `dlopen`s `libkrunfw`, which a static binary cannot do.
- **How a missing syscall shows.** With `KILL_PROCESS` the VMM dies with `SIGSYS` and nothing names the call
  (the audit record needs root). With `ERRNO` the call fails and the runtime reports whatever that causes, far
  from the cause. The diagnosable forms are `SECCOMP_RET_TRAP` with libkrun's handler registered (it logs the
  syscall number and exits; needs the fork) or a user-notification supervisor that names the call (the prototype,
  Linux 5.5 or later; 5.6 for the `pidfd_getfd` hand-off it uses).
- **Drift.** What changes the set: a new msb or libkrun version, a new Rust standard library, a new host glibc.
  The vm-linux job is the drift test once the filter is in the runtime: the prototype ran it in deny mode, so a
  missing call fails a test and the supervisor names it. It only covers the CI runner's glibc.

## 3. no_new_privs, Landlock and namespaces

- **`no_new_privs`.** No side effect found: the runtime runs no set-user-id helper (no `newuidmap`,
  `fusermount` or `sudo`; `sudo` in the guest is the guest's own). Landlock needs it (or `CAP_SYS_ADMIN`).
- **Landlock per kernel.** ABI 1 arrived in Linux 5.13, 2 in 5.19 (`REFER`), 3 in 6.2 (`TRUNCATE`), 4 in 6.7
  (TCP bind and connect), 5 in 6.10 (device `ioctl`), 6 in 6.12 (abstract Unix socket and signal scoping), 7 in
  6.15 (audit logging); later ABIs add pathname Unix sockets (9) and UDP (10). Measured: this Debian/Proxmox
  host has it **off** (`EOPNOTSUPP`); WSL2 6.6 has ABI 3; the Ubuntu runner (7.0) has ABI 8. Ubuntu enables it by
  default since 22.04 (its first kernel, 5.15, has ABI 1: file rules only). A distribution or a custom kernel can leave it out of
  the LSM list.
- **What the ruleset needs** (passing on the Ubuntu runner): read and execute on `/usr`, `/lib`, `/lib64`, `/etc`,
  `/proc`, `/sys` and the runtime folder; read on `/dev/urandom`; read and write on `/dev/null`, `/dev/shm`
  and the msb home; device `ioctl` on `/dev/kvm`; per launch, every path the launch config names (the database,
  log, runtime, run and sandbox folders, `libkrunfw`, each mount source, read-only when the mount is), and
  **read-directory on `/`**, because the file server opens `/` as a directory. TCP bind and connect handled with
  nothing allowed; abstract Unix sockets and signals scoped. Two inputs exist only at launch: the msb home and
  the mount sources (a checkout's `.git` can be anywhere). The prototype parsed them out of the launch config;
  msb itself has them as typed fields.
- **The trial-and-error, on CI.** Each step was one CI run: the config descriptor is `--config-fd 96`, not 3 (146
  tests failed); `/` must be listable (130 failed, all with `trampoline rootfs: Permission denied`); mounts are
  `tag:/host/path[:ro]` strings (7 failed); the doctor copies the runtime binary and clears the environment (2
  failed). No failure named Landlock.
- **Without Landlock** the prototype logs "unavailable" and starts with seccomp and `no_new_privs` only; the whole
  host suite ran that way. A kernel with a lower ABI gets the rights it knows (the best-effort rule of the kernel
  documentation): on ABI 3 or less there is no network rule, no device `ioctl` rule and no scoping.
- **User and mount namespaces** (an alternative, not prototyped). A private mount namespace could show the VMM
  only its own folders, which closes the directory listing and the Unix-socket gaps below. But unprivileged user
  namespaces are restricted on Ubuntu 23.10 and later: an unconfined process gets a namespace without
  capabilities unless an AppArmor profile allows it, and that profile must be installed by root. `/dev/kvm` and
  the share folders would have to be bind-mounted in, and file ownership inside a user namespace changes what the
  file server sees as owner. Not measured.

## 4. Where it goes and who owns it

| Option | Effort | Upstream | After an msb upgrade | Notes |
|---|---|---|---|---|
| Launcher binary in place of `msb` (the prototype) | 2 to 3 days | none needed | the wrapper stays; its launch-config parsing can drift | must copy the `.msbver` section at build time (a script wrapper changes the launch contract); must be found by the doctor, which copies the runtime binary and clears the environment; must allow `execve`; one filter for all threads |
| Patch in the msb fork, start of `msb machine` | 3 to 4 days | likely (msb has no sandbox; as an opt-in) | rebased with the fork | the launch config is typed, so the ruleset is exact; can register libkrun's `SIGSYS` handler, add a second filter after start (no `execve`) and narrow vCPU threads; covers the doctor's and the CLI's VMM too |
| Patch in the libkrun fork, per thread | 2 days on top of the msb patch | possible (Firecracker-style filters) | rebased with the fork | seccomp only; libkrun does not know the paths |
| SDK spawn hook (`pre_exec` in the msb SDK) applying rules Puddle computes | 2 to 3 days | possible | rebased with the fork | no second binary; Puddle owns the ruleset; still one filter, `execve` allowed |
| `systemd-run --user` with `SystemCallFilter`, `ProtectHome` and the like | not viable | n/a | n/a | needs a systemd user instance (none in many containers); the SDK passes descriptors (config, locks, pipes) and checks the child's process id, which a transient unit breaks |

The Windows study (`2026-11-vm-sandbox-windows.md`) reached the same seam: the SDK spawn path is where both
platforms need a hook.

## 5. What it costs

Two machines, the same five guest workloads per variant, interleaved per round (alpine 3.20, 2 vCPUs, 1 GiB; each
time is a whole `msb run` including boot): `boot` (`true`), `fs-read` (`dd` of a 1 GiB file from a read-only
share), `fs-meta` (`ls -l` of 20,000 files on the share and `cat` of 11,111 of them), `blk-write` (1 GiB to the
root disk with `fsync`), `build` (copy, tar, gzip and untar `/usr` on the root disk; light, because alpine's `/usr` is small). Medians in seconds:

| Workload | Ubuntu runner: none | seccomp + nnp | + Landlock | Debian host: none | seccomp + nnp |
|---|---|---|---|---|---|
| boot | 1.18 | 1.17 (−0.7 %) | 1.21 (+2.2 %) | 1.83 | 1.67 (−8.5 %) |
| fs-read | 4.61 | 4.60 (−0.2 %) | 4.60 (−0.2 %) | 3.06 | 3.29 (+7.3 %) |
| fs-meta | 6.72 | 6.86 (+2.2 %) | 6.84 (+1.8 %) | 6.05 | 6.16 (+1.8 %) |
| blk-write | 7.65 | 7.68 (+0.5 %) | 7.65 (0.0 %) | 7.85 | 7.79 (−0.9 %) |
| build | 1.23 | 1.23 (+0.3 %) | 1.24 (+0.9 %) | 1.68 | 1.66 (−1.4 %) |

- **The runner** (4 vCPUs, quiet, 5 rounds) is the clean measurement: every range is within 3 % of the base except
  `fs-meta`, where the seccomp variants are 1.8 to 2.2 % slower and the ranges do not overlap (6.70 to 6.81 against
  6.83 to 6.90). That workload is all virtio-fs requests, so it makes the most host system calls per second.
- **The host** (15 rounds) shares its 40 cores with every other check of the project (load 10 to 18); its spread is
  ±20 % and the medians move both ways. It shows no slowdown above its noise, nothing finer.
- **Per call.** A tight loop of `getpid`, `ioctl(FIONREAD)`, `open`+`close` and `pread` before and after the
  105-entry filter: +4 to 6 ns a call on WSL2 (`getpid` 54 → 59 ns), below the run-to-run noise on the host (its
  syscalls cost 650 to 840 ns inside the container, which already has a filter of its own). Landlock on WSL2
  (ABI 3): +190 ns per `open` of a file 14 directories deep (about +19 %), paid on each path lookup the file
  server makes; the runner's `fs-meta` shows it costs nothing beyond seccomp there.
- **Memory and CPU** of the VMM on the host (`VmHWM`, user + system time): identical within 3 % per workload
  (`fs-meta` 174 MB and 2.83 s against 174 MB and 2.81 s). The prototype's kill mode has no supervisor process;
  the logging supervisor of deny mode is idle unless a call is refused.
- **Start-up**: the wrapper's own work (rules, filter) took 0.5 to 1.4 ms per launch.
- Kernels before 5.16 turn on the speculative store bypass mitigation for every seccomp process by default
  (`spec_store_bypass_disable=seccomp`); that costs CPU-bound code more than the filter itself. The filter can opt
  out with `SECCOMP_FILTER_FLAG_SPEC_ALLOW`. The kernels measured here (6.6, 7.0) default to `prctl` and do not.

## 6. What it protects against

Assume a guest has code execution in the VMM. With the prototype's rules:

| Asset | Today | seccomp + nnp | + Landlock (ABI 8, as prototyped) |
|---|---|---|---|
| `~/.ssh`, browser profiles, any user file outside the rules | read and write | read and write | **refused** (verified on WSL2: `cat` of a file outside the rules is `EACCES`) |
| Puddle's token file and data folder (outside the msb home) | read and write | read and write | **refused** |
| Names of every file on the machine | listed | listed | **still listed** (read-directory on `/`) |
| Other workspaces' disks and the shared msb database | read and write | read and write | **still read and write** (one msb home for all) |
| The credential store (Secret Service over D-Bus), ssh-agent, a Docker socket, Puddle's own sockets | reachable | reachable | **still reachable** (pathname Unix sockets need ABI 9) |
| Other processes (signals, `ptrace`, `/proc/<pid>/mem`) | yes | `ptrace` refused; `/proc/<pid>/mem` left to Yama | signals scoped; tracing access to processes outside the sandbox refused |
| Network: TCP / UDP | yes | yes | TCP refused / UDP still open |
| Kernel attack surface: `bpf`, `io_uring`, `userfaultfd`, `keyctl`, `mount`, `unshare` | open | **refused** | refused |
| Running another program | yes | `execve` allowed (launcher) | only from the folders with the execute right: the system folders, and in this prototype also the writable msb home (a real ruleset drops execute there) |

**Escape chains from the earlier VM-escape review.** The guest-written root-disk journal attaches a host file as
a writable disk: the runtime opens that file, so Landlock refuses a file outside the msb home but not another
workspace's disk inside it. The balloon range bug and the device panics end in VMM code execution, which the table
above bounds. The resource floods are unchanged (they are the runtime's own files and memory).

**Residual risk.** The shared msb home and database; pathname Unix sockets (D-Bus can run commands outside the
sandbox through `systemd --user`); directory names; UDP; the 105 allowed syscalls with free arguments (`ioctl`
included unless narrowed); a kernel bug reachable through `/dev/kvm`; and every kernel without Landlock, which gets
seccomp only.

## 7. Prototype runs

| Run | Where | Mode | Result |
|---|---|---|---|
| strace, whole suite | Debian host | trace | 172 of 176 pass; fails: vsock 256-download stall test, flood caps (slow under strace), doctor ×2 (exit 111 from strace's attach) |
| seccomp deny + nnp, whole suite | Debian host (no Landlock) | user-notify, `EPERM` | 173 of 176 pass; **no syscall of the runtime denied**; the 3 failures are the doctor (this early wrapper confined the CLI too, so the doctor's copy of the wrapper was refused `landlock_create_ruleset`, `seccomp` and `getppid` inside the outer filter); Landlock logged "unavailable" on every launch and the runtime started without it |
| seccomp kill + nnp, whole suite | Debian host (no Landlock) | `KILL_PROCESS` | **176 of 176 pass** in 7473 s; all 304 `machine` launches logged `default kill` and "landlock: unavailable"; the doctor's CLI ran unconfined (only `msb machine` is wrapped) |
| seccomp log + Landlock, iterations | Ubuntu runner | user-notify log | 22 → 38 → 161 → 165 of 168 as the ruleset grew (section 3) |
| seccomp deny + Landlock, final allowlist | Ubuntu runner | user-notify, `EPERM` | 166 of 168; no syscall denied; the 2 failures are the doctor's copy of the wrapper without its config |
| seccomp kill + Landlock | Ubuntu runner, two runs | `KILL_PROCESS` | **168 of 168 pass, both runs**; all 295 `machine` launches logged `landlock: ABI 8 enforced` and `default kill`; the doctor's CLI ran unconfined and its VMM confined |

Calls the supervisor saw outside the allowlist, over every run: in log mode on CI, `renameat2`, `getuid`,
`fallocate`, `shutdown`, `mremap`, `clock_nanosleep`, `syncfs`, `restart_syscall`, `getsockopt`, `listxattr`
before the allowlist took the strace union (all but `listxattr` were in the host trace); with the final allowlist,
none from the runtime on either machine.

## Verdict: DRAWBACKS

| # | Drawback | Severity | Workaround researched | How well proven | Open |
|---|---|---|---|---|---|
| 1 | Landlock, the part that protects the user's files, is off or partial on kernels Puddle runs on: off on the Debian/Proxmox test host, ABI 3 on WSL2 6.6 (no network rules, no device `ioctl` rule, no scoping), ABI 1 on Ubuntu 22.04 | high | best effort per ABI; seccomp and `no_new_privs` everywhere; Puddle names "the VM runs without file confinement on this kernel" as a problem and in the doctor, never a failed start | measured: the whole host suite passed with Landlock unavailable (176 of 176 in kill mode); ABI 3 and 8 measured | how many users run without it; the problem's wording and whether it is a setting |
| 2 | A Landlock denial is a plain `Permission denied` far from its cause; four CI rounds were needed to find the rules | medium | build the ruleset inside msb from the typed launch config (not by parsing it); a Puddle check before start that each mount source is inside the rules; Landlock's audit log (ABI 7) needs root | measured (CI rounds above) | none |
| 3 | The rules that pass let the VMM list every directory on the machine (read-directory on `/`), because the file server opens `/` as a directory | medium | fork patch: open the base with `O_PATH`, which `openat2` accepts as a directory and Landlock does not check | code read (`passthroughfs/unix/mod.rs`, `open_root_no_symlink`); not tried | the patch and a test |
| 4 | Pathname Unix sockets stay reachable until Landlock ABI 9: the session D-Bus (and through it `systemd --user`, which can start any command outside the sandbox), ssh-agent, a Docker socket, Puddle's own sockets | high | a mount namespace that hides `/run/user` (needs unprivileged user namespaces: restricted by AppArmor on Ubuntu 23.10 and later); an ABI 9 kernel; seccomp cannot filter by path, and the vsock routes need `connect` | not tried | the namespace route, and which kernel brings ABI 9 to users |
| 5 | Every workspace's VMM shares one writable msb home: disks of other workspaces and the shared msb database stay readable and writable | high | per-sandbox rules (own sandbox folder writable, image cache read-only) need msb to stop writing shared state from the runtime (the database, `run/` locks), or a home per workspace | code read and the ruleset that passed; not tried | an msb design change; the size of it |
| 6 | The syscall set follows the host glibc and the guest: one call (`listxattr`) appeared only when a guest listed attributes on CI | medium | derive the allowlist from the file server's request handlers plus the traces; a superset for glibc fallbacks (`clone`, `newfstatat`); a filter that names the call (user notification or libkrun's `SIGSYS` handler); vm-linux in deny mode as the drift test; a distribution matrix for glibc | measured on glibc 2.39 and 2.41 | Ubuntu 22.04, Fedora, Arch, arm64 |
| 7 | A wrapper binary breaks what inspects the runtime binary: the SDK's version section (a script wrapper changes the launch contract), the doctor (copies the binary, clears the environment) | medium | apply the controls inside `msb machine` (fork patch) or through an SDK spawn hook, not as a wrapper | measured (CI runs 43138168063, 43141914756; the shell-wrapper path from the SDK source) | none |
| 8 | A filter applied before `exec` must allow `execve` and gives every thread the union | low | a second, narrower filter inside msb after start (no `execve`; 23 calls on vCPU threads) | code read; not tried | the per-thread split |
| 9 | Cost: up to 2.2 % on the virtio-fs metadata workload (above the 3 % bar nowhere, but not zero) | low | `SECCOMP_FILTER_FLAG_SPEC_ALLOW` on kernels before 5.16 | measured on the runner (5 rounds) and the host (15 rounds, noisy) | Landlock's cost with deep share paths |
| 10 | Not measured: Ubuntu 22.04 (ABI 1), Fedora, Arch, arm64, a guest kernel with modules, the user-namespace route | unknown | the degrade path of drawback 1 | not measured | all of it |

**What could start now without these drawbacks** (an implementation row decides; nothing is built): seccomp with
the allowlist in this document plus `no_new_privs`, applied inside `msb machine` (fork patch) in a mode that names
a refused call, with vm-linux as the drift test. It passed every VM test on both machines, costs 4 to 6 ns per
system call (up to 2.2 % on the metadata-heavy share workload), and needs no kernel feature that might be missing. It does not keep a VMM exploit away from the user's
files; that needs Landlock with drawbacks 3 to 5 solved first, and on kernels without Landlock it needs something
else (namespaces) or nothing can be done.

## Appendix: the allowlist (x86_64)

105 calls, the union over the host suite under strace and the CI runs; `clone`, `setsid`, `wait4`, `kill`,
`uname` and `getgid` come from the CLI's `msb run` path (the doctor), so a `machine`-only list is a little
shorter (not measured separately).

```
accept4 access arch_prctl bind brk chmod clock_gettime clock_nanosleep clone clone3 close connect
dup dup2 epoll_create1 epoll_ctl epoll_wait eventfd2 execve exit exit_group fallocate fcntl
fdatasync flock fstat fsync ftruncate futex getdents64 geteuid getgid getpeername getpid getrandom
getsockopt gettid getuid getxattr ioctl kill listen listxattr lseek madvise memfd_create mincore
mkdir mkdirat mmap mprotect mremap munmap newfstatat openat openat2 pidfd_open pipe2 poll prctl
pread64 preadv prlimit64 pwrite64 pwritev read readlink readlinkat recvfrom recvmsg removexattr
rename renameat2 restart_syscall rseq rt_sigaction rt_sigprocmask rt_sigreturn sched_getaffinity
sched_yield sendmsg sendto set_robust_list set_tid_address setsid setxattr shutdown sigaltstack
socket socketpair statx symlink sync_file_range syncfs tgkill timerfd_create timerfd_settime umask
uname unlink unlinkat utimensat wait4 write writev
```
