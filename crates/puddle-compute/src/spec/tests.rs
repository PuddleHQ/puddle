// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn p(s: &str) -> GuestPath {
    GuestPath::new(s).unwrap()
}

fn v(s: &str) -> VolumeName {
    VolumeName::new(s).unwrap()
}

fn base() -> SandboxSpec {
    SandboxSpec::new(
        SandboxName::new("box").unwrap(),
        ImageRef::new("alpine").unwrap(),
    )
}

fn reason(spec: &SandboxSpec) -> String {
    match spec.validate() {
        Err(ComputeError::InvalidSpec { reason }) => reason,
        other => panic!("expected InvalidSpec, got {other:?}"),
    }
}

#[test]
fn defaults_are_no_network_default_memory_nothing_attached() {
    let s = base();
    assert_eq!(s.network, NetworkPolicy::None);
    assert_eq!(s.memory, MemoryMib::DEFAULT);
    assert_eq!(s.cpus, None);
    assert!(s.env.is_empty() && s.routes.is_empty() && s.file_mounts.is_empty());
    assert!(s.volumes.is_empty() && s.owned_disks.is_empty());
    s.validate().unwrap();
}

#[test]
fn builders_fill_the_fields() {
    let mut env = GuestEnv::new();
    env.set("A", "1").unwrap();
    let s = base()
        .with_memory(MemoryMib::MIN)
        .with_cpus(2)
        .with_env(&env)
        .with_route(VsockRoute::new(5000, "/run/p.sock"))
        .with_file_mount(FileMount::read_only("/h/boot.sh", p("/puddle/boot.sh")))
        .with_volume(
            VolumeMount::named(v("ws-a"), p("/workspaces/a")).ensure_size(DiskSize::mib(64)),
        )
        .with_owned_disk(OwnedDisk {
            guest: p("/var/lib/docker"),
            size: DiskSize::gib(1),
        });
    assert_eq!(s.memory, MemoryMib::MIN);
    assert_eq!(s.cpus, Some(2));
    assert_eq!(s.env.get("A"), Some("1"));
    assert_eq!(s.routes[0].guest_port, 5000);
    assert_eq!(s.file_mounts[0].host, PathBuf::from("/h/boot.sh"));
    assert_eq!(s.volumes[0].ensure_size, Some(DiskSize::mib(64)));
    assert_eq!(s.owned_disks[0].size.as_mib(), 1024);
    s.validate().unwrap();
}

#[test]
fn disk_size_units() {
    assert_eq!(DiskSize::gib(2).as_mib(), 2048);
    assert_eq!(DiskSize::mib(2048).as_bytes(), 2_147_483_648);
    assert_eq!(DiskSize::gib(u32::MAX).as_mib(), u32::MAX);
    assert_eq!(DiskSize::mib(5).to_string(), "5 MiB");
}

#[test]
fn validate_rejects_bad_routes() {
    assert!(reason(&base().with_route(VsockRoute::new(0, "/x"))).contains("port must not be 0"));
    let twice = base()
        .with_route(VsockRoute::new(5000, "/x"))
        .with_route(VsockRoute::new(5000, "/y"));
    assert!(reason(&twice).contains("routed twice"));
}

#[test]
fn a_directory_mount_is_its_own_kind_and_conflicts_like_a_file_mount() {
    let file = FileMount::read_only("/h/f", p("/cache/f"));
    let dir = FileMount::read_only_dir("/h/d", p("/cache"));
    assert_eq!((file.kind, dir.kind), (MountKind::File, MountKind::Dir));
    assert_eq!(dir.host, PathBuf::from("/h/d"));
    assert!(reason(&base().with_file_mount(dir.clone()).with_file_mount(file)).contains("overlap"));
    let twice = base()
        .with_file_mount(dir.clone())
        .with_file_mount(FileMount::read_only_dir("/h/e", p("/cache")));
    assert!(reason(&twice).contains("overlap"));
    let under_volume = base()
        .with_file_mount(dir)
        .with_volume(VolumeMount::named(v("ws-a"), p("/cache/ws")));
    assert!(reason(&under_volume).contains("overlap"));
    base()
        .with_file_mount(FileMount::read_only_dir("/h/d", p("/cache")))
        .with_file_mount(FileMount::read_only_dir("/h/e", p("/cache2")))
        .validate()
        .unwrap();
}

#[test]
fn a_checkout_is_its_own_kind_and_only_goes_to_a_sandbox_with_no_route() {
    let checkout = FileMount::checkout("/home/u/api/.git", p("/puddle/import/0"));
    assert_eq!(checkout.kind, MountKind::Checkout);
    // flake-ok: a host path stored and compared as a value; nothing is read or written at it
    assert_eq!(checkout.host, PathBuf::from("/home/u/api/.git"));
    base().with_file_mount(checkout.clone()).validate().unwrap();
    let routed = base()
        .with_route(VsockRoute::new(5000, "/run/p.sock"))
        .with_file_mount(checkout.clone());
    assert_eq!(
        reason(&routed),
        "the checkout mounted at /puddle/import/0 may only go to a sandbox with no route"
    );
    // Any route counts, whatever the order the spec was built in.
    let routed_later = base()
        .with_file_mount(checkout.clone())
        .with_route(VsockRoute::new(6000, "/run/q.sock"));
    assert!(reason(&routed_later).contains("no route"));
    // A plain directory mount keeps its routes.
    base()
        .with_route(VsockRoute::new(5000, "/run/p.sock"))
        .with_file_mount(FileMount::read_only_dir("/h/d", p("/cache")))
        .validate()
        .unwrap();
    let overlap = base()
        .with_file_mount(checkout)
        .with_volume(VolumeMount::named(v("ws-a"), p("/puddle/import")));
    assert!(reason(&overlap).contains("overlap"));
}

#[cfg(unix)]
#[test]
fn a_checkout_dir_that_is_a_link_or_no_directory_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    let linked = tmp.path().join("a");
    std::fs::create_dir_all(&linked).unwrap();
    std::os::unix::fs::symlink(&real, linked.join(".git")).unwrap();
    assert_eq!(
        check_checkout_dir(&linked.join(".git")),
        Err("is a link".into())
    );
    let socket = tmp.path().join("b");
    std::fs::create_dir_all(&socket).unwrap();
    let _listener = std::os::unix::net::UnixListener::bind(socket.join(".git")).unwrap();
    assert_eq!(
        check_checkout_dir(&socket.join(".git")),
        Err("is not a directory".into())
    );
}

#[test]
fn validate_rejects_overlapping_mounts() {
    let nested = base()
        .with_file_mount(FileMount::read_only("/h", p("/puddle/boot.sh")))
        .with_volume(VolumeMount::named(v("ws-a"), p("/puddle")));
    assert!(reason(&nested).contains("overlap"));
    let same = base()
        .with_volume(VolumeMount::named(v("ws-a"), p("/w")))
        .with_owned_disk(OwnedDisk {
            guest: p("/w"),
            size: DiskSize::mib(1),
        });
    assert!(reason(&same).contains("overlap"));
    let siblings = base()
        .with_volume(VolumeMount::named(v("ws-a"), p("/w/a")))
        .with_volume(VolumeMount::named(v("ws-b"), p("/w/ab")));
    siblings.validate().unwrap();
}

#[test]
fn validate_rejects_double_volumes_zero_sizes_and_zero_cpus() {
    let twice = base()
        .with_volume(VolumeMount::named(v("ws-a"), p("/a")))
        .with_volume(VolumeMount::named(v("ws-a"), p("/b")));
    assert!(reason(&twice).contains("attached twice"));
    let zero =
        base().with_volume(VolumeMount::named(v("ws-a"), p("/a")).ensure_size(DiskSize::mib(0)));
    assert!(reason(&zero).contains("size 0"));
    let zero_owned = base().with_owned_disk(OwnedDisk {
        guest: p("/d"),
        size: DiskSize::mib(0),
    });
    assert!(reason(&zero_owned).contains("size 0"));
    assert!(reason(&base().with_cpus(0)).contains("cpus"));
    assert!(reason(&base().with_root_disk(DiskSize::mib(0))).contains("root disk"));
}

#[test]
fn a_root_disk_size_is_optional_and_a_real_size_is_valid() {
    assert_eq!(base().root_disk, None, "a new spec leaves the default");
    let sized = base().with_root_disk(DiskSize::gib(16));
    assert_eq!(sized.root_disk, Some(DiskSize::mib(16 * 1024)));
    sized.validate().unwrap();
}
