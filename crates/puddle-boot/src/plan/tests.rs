// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::machine::MACHINE_SETTINGS_GUEST;

fn image(entrypoint: &[&str], cmd: &[&str], path: Option<&str>) -> ImageConfig {
    ImageConfig {
        entrypoint: entrypoint.iter().map(|s| (*s).to_owned()).collect(),
        cmd: cmd.iter().map(|s| (*s).to_owned()).collect(),
        env: path
            .map(|p| vec![("PATH".to_owned(), p.to_owned())])
            .unwrap_or_default(),
        ..ImageConfig::default()
    }
}

fn file(path: &str, contents: &str) -> GuestFile {
    GuestFile::new(GuestPath::new(path).unwrap(), contents.as_bytes().to_vec())
}

fn text(plan: &BootPlan, path: &str) -> String {
    let f = plan
        .files()
        .iter()
        .find(|f| f.path().as_str() == path)
        .unwrap_or_else(|| panic!("no {path}"));
    String::from_utf8(f.contents().to_vec()).unwrap()
}

#[test]
fn puddle_files_come_first_then_providers_in_order() {
    let plan = BootPlan::builder(&image(&[], &["bash"], Some("/x:/bin")))
        .file(file("/etc/b", "b"))
        .files([file("/etc/a", "a")])
        .build()
        .unwrap();
    let paths: Vec<_> = plan.files().iter().map(|f| f.path().as_str()).collect();
    assert_eq!(
        paths,
        [
            ENV_FILE_GUEST,
            PATH_FILE_GUEST,
            GIT_CONFIG_GUEST,
            MACHINE_SETTINGS_GUEST,
            "/etc/b",
            "/etc/a"
        ]
    );
}

#[test]
fn entrypoint_is_chained_only_when_the_image_declares_one() {
    let debian = BootPlan::builder(&image(&[], &["bash"], None))
        .build()
        .unwrap();
    assert_eq!(debian.entrypoint(), None);
    let dind = BootPlan::builder(&image(&["dockerd-entrypoint.sh"], &["--tls=false"], None))
        .build()
        .unwrap();
    assert_eq!(
        dind.entrypoint().unwrap(),
        ["dockerd-entrypoint.sh", "--tls=false"]
    );
    let req = dind.exec_request(Duration::from_secs(9));
    assert_eq!(req.program, "/bin/sh");
    assert_eq!(
        req.args,
        [BOOT_SH_GUEST, "dockerd-entrypoint.sh", "--tls=false"]
    );
    assert_eq!(req.user.as_deref(), Some("root"));
    assert_eq!(req.env.get("PUDDLE_ROOT"), Some(""));
    assert_eq!(req.timeout, Duration::from_secs(9));
    assert_eq!(req.stdin, dind.render());
}

#[test]
fn image_workdir_is_used_only_when_valid_and_chaining() {
    let mut img = image(&["run"], &[], None);
    img.working_dir = Some("/srv/app".into());
    let plan = BootPlan::builder(&img).build().unwrap();
    let rendered = String::from_utf8(plan.render()).unwrap();
    assert!(rendered.contains("puddle_entrypoint_cwd '/srv/app'\n"));
    img.working_dir = Some("relative".into());
    let rendered = String::from_utf8(BootPlan::builder(&img).build().unwrap().render()).unwrap();
    assert!(!rendered.contains("puddle_entrypoint_cwd"));
    let mut no_entry = image(&[], &["bash"], None);
    no_entry.working_dir = Some("/srv".into());
    let rendered =
        String::from_utf8(BootPlan::builder(&no_entry).build().unwrap().render()).unwrap();
    assert!(!rendered.contains("puddle_entrypoint_cwd"));
}

#[test]
fn path_file_restores_the_image_path_quoted() {
    let plan = BootPlan::builder(&image(&[], &[], Some("/usr/local/cargo/bin:/it's")))
        .build()
        .unwrap();
    assert_eq!(
        text(&plan, PATH_FILE_GUEST),
        format!("{GENERATED}export PATH='/usr/local/cargo/bin:/it'\\''s'\n")
    );
    let none = BootPlan::builder(&image(&[], &[], None)).build().unwrap();
    assert!(
        none.files()
            .iter()
            .all(|f| f.path().as_str() != PATH_FILE_GUEST)
    );
}

#[test]
fn env_file_exports_every_variable_quoted() {
    let mut env = GuestEnv::new();
    env.set("HTTPS_PROXY", "http://127.0.0.1:3128").unwrap();
    env.set("ODD", "a'b $c\nd").unwrap();
    let plan = BootPlan::builder(&ImageConfig::default())
        .env(&env)
        .build()
        .unwrap();
    assert_eq!(
        text(&plan, ENV_FILE_GUEST),
        format!(
            "{GENERATED}export HTTPS_PROXY='http://127.0.0.1:3128'\nexport ODD='a'\\''b $c\nd'\n"
        )
    );
}

#[test]
fn git_config_has_fsync_identity_and_no_credential_helper() {
    let without_id = BootPlan::builder(&ImageConfig::default()).build().unwrap();
    assert_eq!(
        text(&without_id, GIT_CONFIG_GUEST),
        format!("{GENERATED}[core]\n\tfsync = added\n\tfsyncMethod = batch\n")
    );
    let as_agent = BootPlan::builder(&ImageConfig::default())
        .session_user(SessionUser::Agent)
        .build()
        .unwrap();
    assert_eq!(
        text(&as_agent, GIT_CONFIG_GUEST),
        format!(
            "{GENERATED}[core]\n\tfsync = added\n\tfsyncMethod = batch\n[safe]\n\tdirectory = *\n"
        ),
        "root may work in the session user's checkouts"
    );
    let id = GitIdentity::new(r#"Ada "the" \Countess"#, "ada@example.org").unwrap();
    assert_eq!(id.name(), r#"Ada "the" \Countess"#);
    assert_eq!(id.email(), "ada@example.org");
    let plan = BootPlan::builder(&ImageConfig::default())
        .git_identity(id)
        .build()
        .unwrap();
    let git = text(&plan, GIT_CONFIG_GUEST);
    assert!(git.ends_with(
        "[user]\n\tname = \"Ada \\\"the\\\" \\\\Countess\"\n\temail = \"ada@example.org\"\n"
    ));
    for f in plan.files() {
        let body = String::from_utf8_lossy(f.contents()).to_lowercase();
        assert!(!body.contains("credential"), "{}", f.path());
    }
}

fn author(name: &str) -> GitIdentity {
    GitIdentity::new(name, &format!("{}@example.org", name.to_lowercase())).unwrap()
}

fn rule(globs: &[&str], name: &str) -> GitAuthorRule {
    GitAuthorRule::new(
        globs.iter().map(|g| (*g).to_owned()).collect(),
        author(name),
    )
    .unwrap()
}

#[test]
fn author_rules_become_include_if_blocks_in_order_each_with_its_own_file() {
    let plan = BootPlan::builder(&ImageConfig::default())
        .git_identity(author("Ada"))
        .git_author_rule(rule(
            &["https://github.com/**", "https://*@github.com/**"],
            "Bob",
        ))
        .git_author_rule(rule(&["https://github.com/acme/**"], "Cy"))
        .build()
        .unwrap();
    assert_eq!(
        text(&plan, GIT_CONFIG_GUEST),
        format!(
            "{GENERATED}[core]\n\tfsync = added\n\tfsyncMethod = batch\n\
                 [user]\n\tname = \"Ada\"\n\temail = \"ada@example.org\"\n\
                 [includeIf \"hasconfig:remote.*.url:https://github.com/**\"]\n\tpath = git-author-1.gitconfig\n\
                 [includeIf \"hasconfig:remote.*.url:https://*@github.com/**\"]\n\tpath = git-author-1.gitconfig\n\
                 [includeIf \"hasconfig:remote.*.url:https://github.com/acme/**\"]\n\tpath = git-author-2.gitconfig\n"
        )
    );
    assert_eq!(
        text(&plan, "/etc/puddle/git-author-1.gitconfig"),
        format!("{GENERATED}[user]\n\tname = \"Bob\"\n\temail = \"bob@example.org\"\n")
    );
    assert!(text(&plan, "/etc/puddle/git-author-2.gitconfig").contains("\"Cy\""));
    // No rule file without a rule, and still no credential helper anywhere.
    let none = BootPlan::builder(&ImageConfig::default()).build().unwrap();
    assert!(
        none.files()
            .iter()
            .all(|f| !f.path().as_str().contains("git-author"))
    );
    for f in plan.files() {
        let body = String::from_utf8_lossy(f.contents()).to_lowercase();
        assert!(!body.contains("credential"), "{}", f.path());
    }
}

#[test]
fn author_rules_are_checked() {
    let a = author("Ada");
    let globs = |n: usize| (0..n).map(|i| format!("https://h{i}.example/**")).collect();
    for (bad, why) in [
        (GitAuthorRule::new(vec![], a.clone()), "no remote glob"),
        (
            GitAuthorRule::new(globs(MAX_GIT_AUTHOR_GLOBS + 1), a.clone()),
            "too many",
        ),
        (GitAuthorRule::new(vec![String::new()], a.clone()), "empty"),
        (
            GitAuthorRule::new(vec!["a".repeat(1025)], a.clone()),
            "over-long",
        ),
        (
            GitAuthorRule::new(vec!["a\nb".into()], a.clone()),
            "control",
        ),
        (GitAuthorRule::new(vec!["a\"b".into()], a.clone()), "quote"),
        (
            GitAuthorRule::new(vec!["a\\b".into()], a.clone()),
            "backslash",
        ),
    ] {
        let err = bad.unwrap_err();
        assert!(
            matches!(err, PlanError::GitAuthorRule { .. }),
            "{why}: {err}"
        );
        assert!(err.to_string().starts_with("git author rule "), "{err}");
    }
    let ok = GitAuthorRule::new(globs(MAX_GIT_AUTHOR_GLOBS), a).unwrap();
    assert_eq!(ok.globs().len(), MAX_GIT_AUTHOR_GLOBS);
    assert_eq!(ok.author(), &author("Ada"));
    let mut builder = BootPlan::builder(&ImageConfig::default());
    for _ in 0..=MAX_GIT_AUTHOR_RULES {
        builder = builder.git_author_rule(rule(&["https://x.example/**"], "Ada"));
    }
    assert!(matches!(
        builder.build().unwrap_err(),
        PlanError::GitAuthorRule { .. }
    ));
}

#[test]
fn git_identity_rejects_empty_and_control_characters() {
    let cases = [
        ("", "a@b", "user.name is empty"),
        ("a", " ", "user.email is empty"),
        ("a\nb", "a@b", "user.name contains a control character"),
        ("a", "a@b\0", "user.email contains a control character"),
    ];
    for (name, email, want) in cases {
        let err = GitIdentity::new(name, email).unwrap_err();
        assert_eq!(err.to_string(), format!("git {want}"));
    }
}

#[test]
fn plan_refuses_conflicting_or_unusable_files() {
    let build = |f: GuestFile| BootPlan::builder(&ImageConfig::default()).file(f).build();
    assert_eq!(
        build(file(ENV_FILE_GUEST, "x")).unwrap_err(),
        PlanError::DuplicatePath {
            path: ENV_FILE_GUEST.into()
        }
    );
    assert_eq!(
        build(file("/puddle/boot.sh", "x")).unwrap_err(),
        PlanError::ReservedPath {
            path: "/puddle/boot.sh".into()
        }
    );
    let err = build(file("/etc/a\nb", "x")).unwrap_err();
    assert!(matches!(err, PlanError::UnsafePath { .. }));
    assert!(err.to_string().contains("control character"));
    let two = BootPlan::builder(&ImageConfig::default())
        .file(file("/etc/x", "1"))
        .file(file("/etc/x", "2"))
        .build()
        .unwrap_err();
    assert_eq!(two.to_string(), "two boot files claim /etc/x");
}

#[test]
fn plan_refuses_reserved_env_and_bad_image_values() {
    let mut env = GuestEnv::new();
    env.set("PUDDLE_ROOT", "/tmp").unwrap();
    let err = BootPlan::builder(&ImageConfig::default())
        .env(&env)
        .build()
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "environment variable PUDDLE_ROOT is reserved for Puddle's boot hook"
    );
    let err = BootPlan::builder(&image(&[], &[], Some("/a\n/b")))
        .build()
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "the image's ENV PATH contains a NUL or newline"
    );
    let err = BootPlan::builder(&image(&["a\0"], &[], None))
        .build()
        .unwrap_err();
    assert_eq!(
        err,
        PlanError::ImageValue {
            what: "ENTRYPOINT/CMD"
        }
    );
}

#[test]
fn capture_is_on_unless_switched_off_or_there_is_no_agent() {
    let img = image(&["e"], &[], None);
    let render = |plan: &BootPlan| String::from_utf8(plan.render()).unwrap();

    let on = BootPlan::builder(&img).build().unwrap();
    assert!(on.capture());
    assert!(render(&on).contains("puddle_agent '/puddle/puddle-agent' 3128\npuddle_capture on\n"));

    let off = BootPlan::builder(&img).no_capture().build().unwrap();
    assert!(!off.capture());
    assert!(render(&off).contains("\npuddle_capture off\n"));

    let none = BootPlan::builder(&img).no_agent().build().unwrap();
    assert!(!none.capture(), "capture needs the agent");
    assert!(!render(&none).contains("puddle_capture"));
}

#[test]
fn render_has_header_files_steps_and_end_marker() {
    let plan = BootPlan::builder(&image(&["e"], &[], Some("/bin")))
        .file(file("/etc/it's", "50%\n").with_mode(0o600).unwrap())
        .agent(AgentConfig {
            binary: GuestPath::new("/opt/agent").unwrap(),
            port: 4000,
        })
        .build()
        .unwrap();
    let r = String::from_utf8(plan.render()).unwrap();
    let lines: Vec<&str> = r.lines().collect();
    assert_eq!(lines.first(), Some(&PLAN_HEADER));
    assert_eq!(lines.last(), Some(&"puddle_plan_end"));
    assert!(r.contains("puddle_file '/etc/it'\\''s' 0600 '50\\045\\012'\n"));
    assert!(
        r.contains("puddle_on_change '/usr/local/share/ca-certificates' update-ca-certificates\n")
    );
    assert!(r.contains("puddle_git_include '/etc/puddle/gitconfig'\n"));
    assert!(r.contains("puddle_agent '/opt/agent' 4000\n"));
    assert_eq!(lines.get(1), Some(&"puddle_merge_tool '/opt/agent'"));
    assert!(r.contains(&format!(
        "puddle_entrypoint_env '{ENV_FILE_GUEST}'\npuddle_entrypoint_env '{PATH_FILE_GUEST}'\n"
    )));
    assert_eq!(plan.agent().unwrap().port, 4000);
    // Every line is one call: content newlines are escaped.
    assert_eq!(lines.len(), 2 + plan.files().len() + 7);
    // The agent's port is the one VS Code is told to ignore.
    assert!(text(&plan, MACHINE_SETTINGS_GUEST).contains("\"4000\""));
}

#[test]
fn steps_run_after_the_ca_trigger_once_each_and_must_be_plan_files() {
    let step = GuestPath::new("/usr/local/lib/puddle/step.sh").unwrap();
    let other = GuestPath::new("/usr/local/lib/puddle/other.sh").unwrap();
    let plan = BootPlan::builder(&ImageConfig::default())
        .file(file(step.as_str(), "true\n"))
        .file(file(other.as_str(), "true\n"))
        .step(other.clone())
        .step(step.clone())
        .step(other.clone())
        .build()
        .unwrap();
    assert_eq!(plan.steps(), [other.clone(), step.clone()]);
    let r = String::from_utf8(plan.render()).unwrap();
    let trigger = r.find("puddle_on_change").unwrap();
    let first = r
        .find("puddle_step '/usr/local/lib/puddle/other.sh'\n")
        .unwrap();
    let second = r
        .find("puddle_step '/usr/local/lib/puddle/step.sh'\n")
        .unwrap();
    assert!(trigger < first && first < second, "{r}");
    assert_eq!(r.matches("puddle_step ").count(), 2);

    let err = BootPlan::builder(&ImageConfig::default())
        .step(step.clone())
        .build()
        .unwrap_err();
    assert_eq!(
        err,
        PlanError::StepNotInPlan {
            path: step.to_string()
        }
    );
    assert!(err.to_string().contains("/usr/local/lib/puddle/step.sh"));
}

#[test]
fn agent_settings_follow_the_agent_line_quoted() {
    let plan = BootPlan::builder(&ImageConfig::default())
        .agent_env("PUDDLE_AGENT_EDITOR_DIR", "/puddle/code-server")
        .agent_env("PUDDLE_AGENT_EDITOR_DATA", "/w/it's here")
        .build()
        .unwrap();
    let r = String::from_utf8(plan.render()).unwrap();
    assert!(
        r.contains(
            "puddle_agent '/puddle/puddle-agent' 3128\n\
                 puddle_agent_env 'PUDDLE_AGENT_EDITOR_DIR' '/puddle/code-server'\n\
                 puddle_agent_env 'PUDDLE_AGENT_EDITOR_DATA' '/w/it'\\''s here'\n"
        ),
        "{r}"
    );
    assert_eq!(plan.agent_env().len(), 2);
}

#[test]
fn unusable_agent_settings_are_refused_by_name() {
    let build = |env: &[(&str, &str)], agent: bool| {
        let mut b = BootPlan::builder(&ImageConfig::default());
        for (k, v) in env {
            b = b.agent_env(*k, *v);
        }
        if !agent {
            b = b.no_agent();
        }
        b.build().unwrap_err().to_string()
    };
    for (env, why) in [
        (vec![("HTTP_PROXY", "x")], "PUDDLE_AGENT_ name"),
        (vec![("PUDDLE_AGENT_", "x")], "PUDDLE_AGENT_ name"),
        (vec![("PUDDLE_AGENT_x", "x")], "PUDDLE_AGENT_ name"),
        (vec![("PUDDLE_AGENT_A", "a\nb")], "NUL or newline"),
        (vec![("PUDDLE_AGENT_A", "a\0b")], "NUL or newline"),
        (
            vec![("PUDDLE_AGENT_A", "1"), ("PUDDLE_AGENT_A", "2")],
            "set twice",
        ),
    ] {
        let err = build(&env, true);
        assert!(err.contains(why) && err.contains(env[0].0), "{err}");
    }
    let err = build(&[("PUDDLE_AGENT_A", "1")], false);
    assert!(err.contains("starts no agent"), "{err}");
}

#[test]
fn no_agent_leaves_the_agent_step_out() {
    let plan = BootPlan::builder(&ImageConfig::default())
        .no_agent()
        .build()
        .unwrap();
    assert!(plan.agent().is_none());
    let r = String::from_utf8(plan.render()).unwrap();
    assert!(!r.contains("puddle_agent "));
    // Merged files still have their tool: the agent binary's default mount.
    assert!(r.contains("puddle_merge_tool '/puddle/puddle-agent'\n"));
    assert!(text(&plan, MACHINE_SETTINGS_GUEST).contains("\"3128\""));
}

#[test]
fn merged_files_render_their_spec_and_are_counted() {
    use puddle_types::{MergeEntry, MergeFormat, MergeSpec};
    let spec = MergeSpec::new(
        MergeFormat::Json,
        vec![MergeEntry::json(&["a"], &serde_json::json!("50%"))],
    )
    .unwrap();
    let merged = GuestFile::merged(GuestPath::new("/root/c.json").unwrap(), spec)
        .with_mode(0o600)
        .unwrap();
    // Every plan merges VS Code's Machine settings.
    let without = BootPlan::builder(&ImageConfig::default()).build().unwrap();
    assert!(without.has_merged_files());
    let plan = BootPlan::builder(&ImageConfig::default())
        .file(merged)
        .build()
        .unwrap();
    assert!(plan.has_merged_files());
    assert_eq!(plan.merge_tool().as_str(), AGENT_GUEST);
    let r = String::from_utf8(plan.render()).unwrap();
    assert!(
            r.contains(
                r#"puddle_merge '/root/c.json' 0600 '{"format":"json","entries":[{"key":["a"],"value":"\134"50\045\134""}]}'"#
            ),
            "{r}"
        );
}

#[test]
fn a_kept_file_renders_as_its_path_alone() {
    let plan = BootPlan::builder(&ImageConfig::default())
        .file(GuestFile::kept(
            GuestPath::new("/w/.puddle/editor/w.code-workspace").unwrap(),
        ))
        .build()
        .unwrap();
    let r = String::from_utf8(plan.render()).unwrap();
    assert!(
        r.contains("\npuddle_keep '/w/.puddle/editor/w.code-workspace'\n"),
        "{r}"
    );
    assert!(!r.contains("puddle_merge '/w/"), "{r}");
}
