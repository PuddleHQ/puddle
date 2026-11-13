// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

struct SecretLogin(String);

impl EditorLogin for SecretLogin {
    fn password(&self) -> BoxFuture<'_, Result<String, LaunchError>> {
        Box::pin(async move { Ok(self.0.clone()) })
    }
}

struct NoWindow;

impl Launcher for NoWindow {
    fn open_desktop<'a>(
        &'a self,
        _: &'a WorkspaceRecord,
        _: &'a DesktopTarget,
    ) -> BoxFuture<'a, Result<(), LaunchError>> {
        Box::pin(async { Ok(()) })
    }
}

fn sized(root: Option<(u64, u64)>, volume: Option<u64>) -> WorkspaceRecord {
    let mut record = WorkspaceRecord::new(
        WorkspaceId::new("demo").unwrap(),
        DisplayName::parse("demo").unwrap(),
        vec![],
    );
    record.root_disk_used_mib = root.map(|(used, _)| used);
    record.root_disk_size_mib = root.map(|(_, size)| size);
    record.disk_size_mib = 1000;
    record.disk_used_mib = volume;
    record
}

#[test]
fn a_root_disk_reset_names_itself_on_the_wire() {
    assert_eq!(
        Operation::ResettingRootDisk.to_string(),
        "resetting_root_disk"
    );
    assert_eq!(Operation::GrowingRootDisk.to_string(), "growing_root_disk");
}

#[test]
fn disk_percent_rounds_down_caps_at_full_and_needs_both_figures() {
    let t = |root, volume| {
        let r = sized(root, volume);
        (
            r.disk_percent(DiskKind::Root),
            r.disk_percent(DiskKind::Volume),
        )
    };
    assert_eq!(t(Some((949, 1000)), Some(999)), (Some(94), Some(99)));
    assert_eq!(t(Some((5000, 1000)), Some(0)), (Some(100), Some(0)));
    assert_eq!(t(None, None), (None, None));
    let mut zero = sized(Some((1, 0)), None);
    assert_eq!(zero.disk_percent(DiskKind::Root), None);
    zero.root_disk_size_mib = Some(1000);
    zero.root_disk_used_mib = None;
    assert_eq!(zero.disk_percent(DiskKind::Root), None);
}

#[test]
fn a_disk_warns_exactly_at_the_threshold_and_an_unknown_one_never() {
    use puddle_settings::DiskWarningPercent;
    let at = |n| DiskWarningPercent::new(n).unwrap();
    let both = sized(Some((950, 1000)), Some(950));
    assert_eq!(
        both.disk_warnings(at(95)),
        [DiskKind::Root, DiskKind::Volume]
    );
    assert_eq!(both.disk_warnings(at(96)), []);
    let root_only = sized(Some((950, 1000)), Some(100));
    assert_eq!(root_only.disk_warnings(at(95)), [DiskKind::Root]);
    let volume_only = sized(Some((100, 1000)), Some(960));
    assert_eq!(volume_only.disk_warnings(at(95)), [DiskKind::Volume]);
    assert_eq!(sized(None, None).disk_warnings(at(50)), []);
    // 100 % warns only on a full disk.
    assert_eq!(
        sized(Some((999, 1000)), Some(1000)).disk_warnings(at(100)),
        [DiskKind::Volume]
    );
}

#[test]
fn a_launcher_with_no_window_has_nothing_to_close_and_nothing_to_forget() {
    let name = DisplayName::parse("demo").unwrap();
    let record = WorkspaceRecord::new(WorkspaceId::new("demo").unwrap(), name.clone(), vec![]);
    let desktop = DesktopTarget {
        ssh_host: "puddle-demo".into(),
        path: "/".into(),
        is_workspace_file: false,
    };
    futures_util::FutureExt::now_or_never(NoWindow.open_desktop(&record, &desktop))
        .unwrap()
        .unwrap();
    futures_util::FutureExt::now_or_never(
        NoWindow.close_browser(&WorkspaceId::new("demo").unwrap()),
    )
    .unwrap();
    futures_util::FutureExt::now_or_never(
        NoWindow.forget_browser(&WorkspaceId::new("demo").unwrap()),
    )
    .unwrap()
    .unwrap();
    // A shell with no window keeps no browser data under any id.
    assert!(!NoWindow.has_browser_data(&WorkspaceId::new("demo").unwrap()));
}

#[test]
fn a_browser_target_shows_where_the_editor_is_and_never_the_way_to_its_password() {
    let secret = "debug-form-secret-value".to_owned();
    let target = BrowserTarget {
        origin: "http://demo.localhost:4100".into(),
        front: "127.0.0.1:4100".parse().unwrap(),
        settings: puddle_settings::resolve(
            &puddle_settings::GlobalSettings::default(),
            None::<&puddle_settings::WorkspaceSettings>,
        ),
        login: Some(EditorSignIn::Form(Arc::new(SecretLogin(secret.clone())))),
        open: None,
    };
    let login = target.login.as_ref().unwrap().reader();
    assert_eq!(
        futures_util::FutureExt::now_or_never(login.password())
            .unwrap()
            .unwrap(),
        secret
    );
    let shown = format!("{target:?}");
    assert!(shown.contains("http://demo.localhost:4100"), "{shown}");
    assert!(shown.contains("127.0.0.1:4100"), "{shown}");
    assert!(!shown.contains(&secret), "{shown}");
    assert!(!shown.contains("SecretLogin"), "{shown}");
}

#[test]
fn https_urls_pass() {
    for ok in [
        "https://github.com/acme/api",
        "https://github.com/acme/api.git",
        "HTTPS://GitHub.com/acme/api",
        "https://dev.azure.com/org/proj/_git/repo",
        "https://git.example.com:8443/a/b",
        "  https://github.com/acme/api  ",
    ] {
        assert_eq!(RepoUrl::parse(ok).unwrap().as_str(), ok.trim());
    }
}

#[test]
fn ssh_remotes_get_the_not_supported_message() {
    for ssh in [
        "git@github.com:acme/api.git",
        "ssh://git@github.com/acme/api.git",
        "SSH://github.com/acme/api",
        "github.com:acme/api",
        "user@host:path",
    ] {
        let err = RepoUrl::parse(ssh).unwrap_err().to_string();
        assert!(
            err.contains("ssh remotes are not supported yet"),
            "{ssh}: {err}"
        );
        assert!(err.contains("HTTPS"), "{err}");
    }
}

#[test]
fn other_urls_say_what_to_change() {
    let cases = [
        ("http://github.com/a/b", "https://"),
        ("git://github.com/a/b", "https://"),
        ("/home/me/repo", "https://"),
        ("C:\\repos\\api", "https://"),
        ("file:///repo", "https://"),
        ("", "https://"),
        ("https://", "no host"),
        ("https:///path", "no host"),
        ("https://:443/x", "no host"),
        (
            "https://user:secret@github.com/a/b",
            "user name and password",
        ),
        ("https://token@github.com/a/b", "user name and password"),
        ("https://github.com/a b", "spaces"),
        ("https://github.com/a\nb", "control"),
    ];
    for (url, wants) in cases {
        let err = RepoUrl::parse(url).unwrap_err().to_string();
        assert!(err.contains(wants), "{url:?}: {err}");
        assert!(!err.contains("secret"), "{err}");
    }
    let long = format!("https://github.com/{}", "a".repeat(MAX_REPO_URL_LEN));
    assert!(
        RepoUrl::parse(&long)
            .unwrap_err()
            .to_string()
            .contains("longer than")
    );
}

#[test]
fn a_check_is_clean_only_without_findings_or_errors() {
    let id = WorkspaceId::new("w").unwrap();
    let mut check = DeleteCheck {
        workspace: id,
        repos: vec![RepoFindings::default()],
        other: Listing::default(),
        errors: vec![],
        removes_sandbox: None,
        volume_missing: false,
        fingerprint: String::new(),
    };
    assert!(check.is_clean());
    check.repos[0].stashes.more = 1;
    assert!(!check.is_clean());
    check.repos[0].stashes.more = 0;
    check.other.items.push("notes.txt".into());
    assert!(!check.is_clean());
    check.other = Listing::default();
    check.errors.push("repo: unreadable".into());
    assert!(!check.is_clean());
}

#[test]
fn a_missing_volume_changes_the_fingerprint() {
    let id = WorkspaceId::new("w").unwrap();
    let plain = DeleteCheck::new(id, vec![], Listing::default(), vec![], None);
    let gone = plain.clone().with_volume_missing();
    assert!(gone.volume_missing && gone.is_clean());
    assert_ne!(plain.fingerprint, gone.fingerprint);
}

#[tokio::test]
async fn the_empty_service_says_it_is_unavailable() {
    let svc = NoWorkspaces;
    let id = WorkspaceId::new("w").unwrap();
    let name = DisplayName::parse("w").unwrap();
    let dir = CheckoutDir::new("a").unwrap();
    let entry = EntryId::from_random([1; 5]);
    let url = RepoUrl::parse("https://example.com/a").unwrap();
    let repos = vec![NewRepository {
        repository: Repository::at_default_directory(EntryId::from_random([1; 5]), url),
        identity: None,
        pull: true,
        push: true,
        local: None,
    }];
    let all = [
        svc.list().await.map(|_| ()),
        svc.get(&id).await.map(|_| ()),
        svc.create(NewWorkspace::new(UserId::OWNER, name, repos), &NoGitSetup)
            .await
            .map(|_| ()),
        svc.start(&id).await.map(|_| ()),
        svc.stop(&id).await.map(|_| ()),
        svc.reclaim(&id).await.map(|_| ()),
        svc.reset_root_disk(&id).await.map(|_| ()),
        svc.delete_check(&id).await.map(|_| ()),
        svc.delete(&id, None).await.map(|_| ()),
        svc.rename(&id, DisplayName::parse("x").unwrap())
            .await
            .map(|_| ()),
        svc.set_session_user(&id, SessionUser::Root)
            .await
            .map(|_| ()),
        svc.attach(&id, AttachMode::Browser).await.map(|_| ()),
        svc.checkouts(&id).await.map(|_| ()),
        svc.add_repository(
            &id,
            AddRepository::Existing {
                directory: dir.clone(),
                seen: RepoUrl::parse("https://github.com/o/r").unwrap(),
                identity: None,
                pull: true,
                push: true,
            },
            &NoGitSetup,
        )
        .await
        .map(|_| ()),
        svc.clone_again(&id, &entry, &NoGitSetup).await.map(|_| ()),
        svc.remove_repository(&id, &entry).await.map(|_| ()),
    ];
    for result in all {
        assert!(matches!(result, Err(WorkspaceError::Unavailable(_))));
    }
}

#[tokio::test]
async fn a_service_with_no_credential_store_keeps_no_logins_to_list_or_forget() {
    let id = WorkspaceId::new("w").unwrap();
    let listed = NoWorkspaces.kept_logins(&id).await;
    assert!(
        matches!(listed, Err(WorkspaceError::Unavailable(ref why)) if why.contains("no logins")),
        "{listed:?}"
    );
    let forgotten = NoWorkspaces.forget_login(&id, "claude").await;
    assert!(
        matches!(forgotten, Err(WorkspaceError::Unavailable(ref why)) if why.contains("no logins")),
        "{forgotten:?}"
    );
}

#[tokio::test]
async fn a_service_with_no_guest_does_not_check_shell_init() {
    let id = WorkspaceId::new("w").unwrap();
    let check = NoWorkspaces.check_shell_init(&id, "echo hi\n").await;
    assert!(
        matches!(check, Ok(ShellInitCheck::NotRun(ref why)) if why.contains("no workspace")),
        "{check:?}"
    );
}

#[test]
fn display_names_are_trimmed_and_bounded() {
    assert_eq!(
        DisplayName::parse("  Web shop ").unwrap().as_str(),
        "Web shop"
    );
    assert_eq!(
        DisplayName::parse("Café 日本").unwrap().as_str(),
        "Café 日本"
    );
    assert!(DisplayName::parse("a".repeat(60).as_str()).is_ok());
    for (bad, wants) in [
        ("", "empty"),
        ("   ", "empty"),
        (&"a".repeat(61), "longer than 60"),
        ("two\nlines", "line breaks"),
        ("tab\there", "control"),
    ] {
        let err = DisplayName::parse(bad).unwrap_err().to_string();
        assert!(err.contains(wants), "{bad:?}: {err}");
    }
    // Sixty multi-byte characters are within the limit: it counts characters, not bytes.
    assert!(DisplayName::parse(&"é".repeat(60)).is_ok());
}

#[test]
fn a_label_is_taken_ignoring_case_and_the_workspace_itself() {
    let mk = |id: &str, shown: &str| {
        WorkspaceRecord::new(
            WorkspaceId::new(id).unwrap(),
            DisplayName::parse(shown).unwrap(),
            Vec::new(),
        )
    };
    let all = [mk("alpha", "alpha"), mk("beta", "Web Shop")];
    assert_eq!(all[1].label(), "Web Shop");
    let beta = WorkspaceId::new("beta").unwrap();
    assert_eq!(label_taken(&all, "web shop", None).unwrap().id, beta);
    assert_eq!(
        label_taken(&all, "ALPHA", None).unwrap().name.as_str(),
        "alpha"
    );
    assert!(label_taken(&all, "web shop", Some(&beta)).is_none());
    // The id is no name: nothing goes by "beta".
    assert!(label_taken(&all, "beta", None).is_none());
}

#[test]
fn an_id_for_a_name_is_a_label_the_same_every_time() {
    let id = |s: &str| {
        id_for_name(&DisplayName::parse(s).unwrap())
            .unwrap()
            .to_string()
    };
    assert_eq!(id("Shop API \u{fc}"), "shop-api");
    assert_eq!(id("  --My  Project__2--"), "my-project-2");
    assert_eq!(id("acme"), "acme");
    let long = id(&"ab ".repeat(20));
    assert!(
        long.len() <= WorkspaceId::MAX_LEN && !long.ends_with('-'),
        "{long}"
    );
    for odd in ["\u{65e5}\u{672c}", "!!!", "tauri", "ipc"] {
        let one = id(odd);
        assert!(
            one.starts_with("workspace-") && one.len() == 18,
            "{odd}: {one}"
        );
        assert_eq!(one, id(odd));
    }
    assert_ne!(id("\u{65e5}\u{672c}"), id("\u{4e2d}\u{6587}"));
}

#[test]
fn the_name_source_numbers_its_later_draws_and_cuts_the_label_for_the_number() {
    let new = |name: &str| {
        NewWorkspace::new(UserId::OWNER, DisplayName::parse(name).unwrap(), Vec::new())
    };
    let taken = |ids: &'static [&'static str]| move |id: &WorkspaceId| ids.contains(&id.as_str());
    assert_eq!(
        new("Shop").id(&NameIds, taken(&[])).unwrap().as_str(),
        "shop"
    );
    assert_eq!(
        new("Shop").id(&NameIds, taken(&["shop"])).unwrap().as_str(),
        "shop-2"
    );
    assert_eq!(
        new("shop!")
            .id(&NameIds, taken(&["shop", "shop-2"]))
            .unwrap()
            .as_str(),
        "shop-3"
    );
    // The number fits under the longest id: the label is cut, never the number.
    let long = "a".repeat(DisplayName::MAX_CHARS);
    let full = WorkspaceId::new(&long).unwrap();
    let second = new(&long).id(&NameIds, |id| *id == full).unwrap();
    assert_eq!(second.as_str().len(), WorkspaceId::MAX_LEN);
    assert!(second.as_str().ends_with("-2"), "{second}");
}

/// A source that gives the same id every time, or nothing at all, and counts its draws.
struct Stuck {
    id: Option<WorkspaceId>,
    draws: std::sync::atomic::AtomicU32,
}

impl IdSource for Stuck {
    fn draw(&self, _name: &DisplayName, attempt: u32) -> Result<WorkspaceId, IdDrawError> {
        assert_eq!(
            attempt,
            self.draws.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        );
        self.id
            .clone()
            .ok_or(IdDrawError::Random(getrandom::Error::UNSUPPORTED))
    }
}

#[test]
fn an_entry_id_is_drawn_again_while_taken_and_given_up_on_after_sixteen_draws() {
    let calls = std::cell::Cell::new(0_u8);
    let counting = |bytes: &mut [u8; 5]| {
        calls.set(calls.get() + 1);
        *bytes = [calls.get(); 5];
        Ok::<(), getrandom::Error>(())
    };
    let first = EntryId::from_random([1; 5]);
    let id = draw_entry_id(counting, |e| *e == first).unwrap();
    assert_eq!((calls.get(), id), (2, EntryId::from_random([2; 5])));

    let draws = std::cell::Cell::new(0_u32);
    let err = draw_entry_id(
        |_| {
            draws.set(draws.get() + 1);
            Ok::<(), getrandom::Error>(())
        },
        |_| true,
    )
    .unwrap_err()
    .to_string();
    assert_eq!(draws.get(), MAX_ID_DRAWS);
    assert!(err.contains("no free repository entry id"), "{err}");

    let empty = draw_entry_id(|_| Err("no randomness"), |_| false)
        .unwrap_err()
        .to_string();
    assert!(empty.contains("random source may be broken"), "{empty}");
    assert!(new_entry_id(|_| false).is_ok());
}

#[test]
fn a_stuck_source_is_given_up_on_after_sixteen_draws() {
    let new = NewWorkspace::new(
        UserId::OWNER,
        DisplayName::parse("acme").unwrap(),
        Vec::new(),
    );
    let stuck = WorkspaceId::new("a0000000").unwrap();
    for id in [Some(stuck.clone()), None] {
        let source = Stuck {
            id,
            draws: std::sync::atomic::AtomicU32::new(0),
        };
        let err = new.id(&source, |id| *id == stuck).unwrap_err();
        assert_eq!(source.draws.into_inner(), MAX_ID_DRAWS);
        assert_eq!(MAX_ID_DRAWS, 16);
        assert_eq!(
            err.to_string(),
            "no free workspace id was found; the system's random source may be broken"
        );
    }
    // A free id on the last draw is still taken up.
    let source = Stuck {
        id: Some(stuck.clone()),
        draws: std::sync::atomic::AtomicU32::new(0),
    };
    let seen = std::sync::atomic::AtomicU32::new(0);
    let last = new
        .id(&source, |_| {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 < MAX_ID_DRAWS
        })
        .unwrap();
    assert_eq!(last, stuck);
}

#[test]
fn the_random_source_draws_generated_ids_whatever_the_name() {
    let new = NewWorkspace::new(
        UserId::OWNER,
        DisplayName::parse("acme").unwrap(),
        Vec::new(),
    );
    let first = new.id(&RandomIds, |_| false).unwrap();
    assert_eq!(first.as_str().len(), WorkspaceId::GENERATED_LEN);
    assert_ne!(first.as_str(), "acme");
    // A taken id is drawn again, not numbered: the first draw is refused, the second taken up.
    let asked = std::sync::atomic::AtomicU32::new(0);
    let second = new
        .id(&RandomIds, |_| {
            asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0
        })
        .unwrap();
    assert_eq!(asked.into_inner(), 2);
    // Two random draws are not compared: `asked` shows the second came from a new draw.
    assert_eq!(second.as_str().len(), WorkspaceId::GENERATED_LEN);
}

#[test]
fn a_lookup_finds_an_id_or_a_name_ignoring_case_and_counts_a_workspace_once() {
    let mk = |id: &str, name: &str| {
        WorkspaceRecord::new(
            WorkspaceId::new(id).unwrap(),
            DisplayName::parse(name).unwrap(),
            Vec::new(),
        )
    };
    let all = [
        mk("shop", "Shop"),
        mk("k7m2x9qa", "Web Shop \u{fc}"),
        mk("web", "x"),
    ];
    let ids =
        |q: &str| -> Vec<String> { lookup(&all, q).iter().map(|r| r.id.to_string()).collect() };
    assert_eq!(ids("k7m2x9qa"), ["k7m2x9qa"]);
    assert_eq!(ids("  WEB shop \u{dc} "), ["k7m2x9qa"]);
    assert_eq!(
        ids("shop"),
        ["shop"],
        "an id that is also the name is one match"
    );
    assert_eq!(ids("nothing"), Vec::<String>::new());
    // One workspace's id is another's name: two matches.
    assert_eq!(ids("web"), ["web"]);
    let all = [mk("shop", "Shop"), mk("k7m2x9qa", "shop")];
    let found: Vec<_> = lookup(&all, "shop")
        .iter()
        .map(|r| r.id.to_string())
        .collect();
    assert_eq!(found, ["shop", "k7m2x9qa"]);
}

#[test]
fn a_rename_is_refused_while_creating_or_deleting_and_on_a_clash() {
    let mk = |id: &str| {
        WorkspaceRecord::new(
            WorkspaceId::new(id).unwrap(),
            DisplayName::parse(id).unwrap(),
            Vec::new(),
        )
    };
    let (mut a, b) = (mk("alpha"), mk("beta"));
    let name = |s: &str| DisplayName::parse(s).unwrap();
    let ida = a.id.clone();
    let check = |a: &WorkspaceRecord, to: &DisplayName| check_rename([a, &b], &ida, to);
    assert!(check(&a, &name("Web")).is_ok());
    assert!(
        check(&a, &name("alpha")).is_ok(),
        "its own name is not a clash"
    );
    assert!(matches!(
        check(&a, &name("BETA")),
        Err(WorkspaceError::Conflict(m)) if m.contains("already called beta")
    ));
    a.busy = Some(Operation::Creating);
    assert!(
        matches!(check(&a, &name("Web")), Err(WorkspaceError::Conflict(m)) if m.contains("creating"))
    );
    a.busy = Some(Operation::Deleting);
    assert!(matches!(
        check(&a, &name("Web")),
        Err(WorkspaceError::Conflict(_))
    ));
    for busy in [
        Operation::Starting,
        Operation::Stopping,
        Operation::Reclaiming,
        Operation::ResettingRootDisk,
    ] {
        a.busy = Some(busy);
        assert!(check(&a, &name("Web")).is_ok(), "{busy}");
    }
    let missing = WorkspaceId::new("nope").unwrap();
    assert!(matches!(
        check_rename([&a, &b], &missing, &name("x")),
        Err(WorkspaceError::NotFound(_))
    ));

    // The start folder and the session user change on a settled workspace only.
    a.busy = Some(Operation::Creating);
    assert!(matches!(
        check_session_user_change([&a, &b], &ida),
        Err(WorkspaceError::Conflict(m)) if m == "alpha is creating; change the user it runs as when that is done"
    ));
    assert!(matches!(
        check_start_folder_change([&a, &b], &ida),
        Err(WorkspaceError::Conflict(m)) if m.ends_with("change its start folder when that is done")
    ));
    a.busy = Some(Operation::Starting);
    assert!(check_session_user_change([&a, &b], &ida).is_ok());
    assert!(matches!(
        check_session_user_change([&a, &b], &missing),
        Err(WorkspaceError::NotFound(_))
    ));
}

fn draft(url: &str) -> RepositoryDraft {
    RepositoryDraft {
        url: url.to_owned(),
        ..RepositoryDraft::default()
    }
}

#[test]
fn a_checkout_on_this_computer_takes_its_origin_and_folder_name_unless_given() {
    let source = |folder: &str, origin: Result<&str, &str>| LocalSource {
        // flake-ok: only compared as a value, never opened as a path
        git_dir: PathBuf::from(format!("/home/u/{folder}/.git")),
        folder_name: folder.into(),
        origin: origin.map(str::to_owned).map_err(str::to_owned),
    };
    let local = |path: &str, url: &str| RepositoryDraft {
        url: url.to_owned(),
        local: Some(path.to_owned()),
        ..RepositoryDraft::default()
    };
    let mut locals = BTreeMap::new();
    locals.insert(
        "/home/u/Shop".to_owned(),
        Ok(source("Shop", Ok("https://github.com/acme/shop.git"))),
    );
    locals.insert(
        "/home/u/none".to_owned(),
        Ok(source("none", Err("it has no origin remote"))),
    );
    locals.insert(
        "/home/u/plain".to_owned(),
        Err(LocalCheckoutError::NotCheckout {
            // flake-ok: a map key and a message, never read from disk
            folder: PathBuf::from("/home/u/plain"),
        }),
    );
    let checked = check_repositories_with(
        &[
            local("/home/u/Shop", ""),
            local("/home/u/none", "https://example.org/acme/x.git"),
        ],
        |_| false,
        &locals,
    )
    .unwrap();
    assert_eq!(
        checked[0].repository.url.as_str(),
        "https://github.com/acme/shop.git"
    );
    assert_eq!(checked[0].repository.directory.as_str(), "Shop");
    // flake-ok: only compared as a value, never opened as a path
    assert_eq!(checked[0].local, Some(PathBuf::from("/home/u/Shop/.git")));
    // A given address stands in for a missing origin.
    assert_eq!(
        checked[1].repository.url.as_str(),
        "https://example.org/acme/x.git"
    );
    assert!(checked.iter().all(|r| r.pull && r.push));
    let refused = |d: RepositoryDraft| {
        check_repositories_with(&[d], |_| false, &locals)
            .unwrap_err()
            .to_string()
    };
    assert_eq!(
        refused(local("/home/u/none", "")),
        "repository 1 (the checkout /home/u/none): it has no origin remote"
    );
    assert_eq!(
        refused(local("/home/u/plain", "")),
        "repository 1 (the checkout /home/u/plain): /home/u/plain is not a git checkout: it has no .git"
    );
    assert_eq!(
        refused(local("/home/u/unseen", "")),
        "repository 1 (the checkout /home/u/unseen): it was not looked at"
    );
    // The address still passes the create's checks.
    assert!(refused(local("/home/u/Shop", "http://example.org/x")).contains("https://"));
    // Without a draft that names one, the plain check needs no service.
    assert_eq!(
        check_repositories(&[draft("https://github.com/a/b")], |_| false).unwrap()[0].local,
        None
    );
}

#[test]
fn the_default_service_has_no_checkouts_on_this_computer() {
    let err = NoWorkspaces
        .local_checkout("/home/u/shop")
        .unwrap_err()
        .to_string();
    assert!(err.contains("not available"), "{err}");
}

#[test]
fn a_repository_list_takes_defaults_and_keeps_its_order() {
    let checked = check_repositories(
        &[
            draft("https://github.com/acme/api.git"),
            RepositoryDraft {
                directory: Some("site".into()),
                identity: Some(IdentityId(4)),
                pull: Some(false),
                ..draft("https://github.com/acme/web")
            },
        ],
        |id| id == IdentityId(4),
    )
    .unwrap();
    let shape: Vec<_> = checked
        .iter()
        .map(|r| {
            (
                r.repository.url.as_str(),
                r.repository.directory.as_str(),
                r.identity,
                r.pull,
                r.push,
            )
        })
        .collect();
    assert_eq!(
        shape,
        [
            ("https://github.com/acme/api.git", "api", None, true, true),
            (
                "https://github.com/acme/web",
                "site",
                Some(IdentityId(4)),
                false,
                true
            ),
        ]
    );
    assert_eq!(check_repositories(&[], |_| false).unwrap(), []);
}

#[test]
fn a_repository_list_is_refused_naming_the_entry() {
    let refused = |drafts: &[RepositoryDraft]| {
        check_repositories(drafts, |_| false)
            .unwrap_err()
            .to_string()
    };
    let clash = refused(&[
        draft("https://github.com/acme/api"),
        draft("https://github.com/acme/web"),
        draft("https://gitlab.com/other/api.git"),
    ]);
    assert_eq!(
        clash,
        "repositories 1 (https://github.com/acme/api) and 3 (https://gitlab.com/other/api.git) \
             both go into api; give one of them another directory, such as api-2"
    );
    let taken = refused(&[
        draft("https://github.com/acme/api"),
        RepositoryDraft {
            directory: Some("api-2".into()),
            ..draft("https://github.com/acme/web")
        },
        draft("https://gitlab.com/other/api.git"),
    ]);
    assert!(taken.ends_with("such as api-3"), "{taken}");
    let twice = refused(&[
        draft("https://github.com/Acme/API.git"),
        RepositoryDraft {
            directory: Some("again".into()),
            ..draft("https://github.com/acme/api/")
        },
    ]);
    assert_eq!(
        twice,
        "repositories 1 (https://github.com/Acme/API.git) and 2 (https://github.com/acme/api/) \
             are the same repository, github.com/acme/api; list it once"
    );
    let ghost = refused(&[
        draft("https://github.com/acme/api"),
        RepositoryDraft {
            identity: Some(IdentityId(9)),
            ..draft("https://github.com/acme/web")
        },
    ]);
    assert_eq!(
        ghost,
        "repository 2 (https://github.com/acme/web): there is no identity 9"
    );
    let ssh = refused(&[draft("git@github.com:acme/api.git")]);
    assert!(
        ssh.starts_with("repository 1 (git@github.com:acme/api.git): ssh remotes"),
        "{ssh}"
    );
    let bad_dir = refused(&[RepositoryDraft {
        directory: Some("../up".into()),
        ..draft("https://github.com/acme/api")
    }]);
    assert!(bad_dir.contains("must not start with"), "{bad_dir}");
    let many: Vec<_> = (0..=MAX_REPOSITORIES)
        .map(|n| draft(&format!("https://github.com/acme/r{n}")))
        .collect();
    assert_eq!(
        refused(&many),
        "a workspace holds at most 100 repositories; this list has 101"
    );
    assert!(check_repositories(&many[..MAX_REPOSITORIES], |_| false).is_ok());
}

#[test]
fn a_refused_address_is_named_without_its_credentials() {
    let err = check_repositories(&[draft("  https://me:tok@github.com/a/b  ")], |_| false)
        .unwrap_err()
        .to_string();
    assert!(
        err.starts_with("repository 1 (https://***@github.com/a/b): remove the user name"),
        "{err}"
    );
    assert!(!err.contains("tok"), "{err}");
    let long = format!("https://github.com/{}", "a".repeat(3000));
    let err = check_repositories(&[draft(&long)], |_| false)
        .unwrap_err()
        .to_string();
    assert!(err.len() < 400, "{}", err.len());
}

#[test]
fn a_long_directory_still_gets_a_free_name_that_fits() {
    let long = "a".repeat(CheckoutDir::MAX_LEN);
    let refused = check_repositories(
        &[
            RepositoryDraft {
                directory: Some(long.clone()),
                ..draft("https://github.com/acme/one")
            },
            RepositoryDraft {
                directory: Some(long.clone()),
                ..draft("https://github.com/acme/two")
            },
        ],
        |_| false,
    )
    .unwrap_err()
    .to_string();
    let suggested = refused.rsplit(' ').next().unwrap();
    assert_eq!(suggested, format!("{}-2", "a".repeat(98)));
}

#[test]
fn a_start_folder_is_a_plain_path_under_the_volume() {
    for ok in ["api", "api/src", "web site/app", "a.b/c_d"] {
        assert_eq!(StartFolder::parse(ok).unwrap().as_str(), ok);
    }
    assert!(StartFolder::parse(&"a".repeat(255)).is_ok());
    for (bad, why) in [
        ("", "empty"),
        ("/api", "leading /"),
        ("api/", "empty, . or .."),
        ("a//b", "empty, . or .."),
        ("./api", "empty, . or .."),
        ("api/../..", "empty, . or .."),
        ("..", "empty, . or .."),
        ("a\\b", "control characters"),
        ("a\nb", "control characters"),
        (".puddle", "inside .puddle"),
        (".puddle/editor", "inside .puddle"),
        ("lost+found/x", "inside lost+found"),
        (&"a".repeat(256), "255 bytes"),
    ] {
        let err = StartFolder::parse(bad).unwrap_err().to_string();
        assert!(err.contains(why), "{bad:?}: {err}");
    }
    assert_eq!(
        StartFolder::of_checkout(&CheckoutDir::new("src").unwrap()).as_str(),
        "src"
    );
}

#[test]
fn operations_have_wire_names() {
    assert_eq!(Operation::Reclaiming.to_string(), "reclaiming");
    assert_eq!(Operation::Deleting.to_string(), "deleting");
}

#[test]
fn an_error_made_from_a_cause_says_the_causes_message() {
    assert_eq!(
        WorkspaceError::conflict("it is running"),
        WorkspaceError::Conflict("it is running".to_owned())
    );
    assert_eq!(WorkspaceError::invalid("bad name").to_string(), "bad name");
    assert_eq!(
        WorkspaceError::unavailable("no runtime").to_string(),
        "no runtime"
    );
    assert_eq!(
        WorkspaceError::internal("disk gone"),
        WorkspaceError::Internal("disk gone".to_owned())
    );
}
