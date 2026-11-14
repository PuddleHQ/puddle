// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::BTreeMap;

use puddle_api::PublicHost;

use super::*;

/// What a test sets an option to from each place, so a winner is told by its value.
struct Samples {
    flag: &'static str,
    env: &'static str,
    file: &'static str,
}

fn samples(kind: Kind) -> Samples {
    match kind {
        Kind::Port => Samples {
            flag: "1111",
            env: "2222",
            file: "3333",
        },
        Kind::Path => Samples {
            flag: "from-flag",
            env: "from-env",
            file: "from-file",
        },
        Kind::Log => Samples {
            flag: "debug",
            env: "warn",
            file: "error",
        },
        // Two values only: the source tells the winner where the value cannot.
        Kind::Bool => Samples {
            flag: "true",
            env: "false",
            file: "true",
        },
        Kind::Ip => Samples {
            flag: "10.0.0.1",
            env: "10.0.0.2",
            file: "10.0.0.3",
        },
        Kind::IpList => Samples {
            flag: "10.0.1.1,10.0.1.2",
            env: "10.0.2.1",
            file: "10.0.3.1",
        },
        Kind::HostList => Samples {
            flag: "a.example.org,b.example.org:8443",
            env: "c.example.org",
            file: "d.example.org",
        },
        Kind::Domain => Samples {
            flag: "flag.example.net",
            env: "env.example.net",
            file: "file.example.net",
        },
    }
}

/// The config file line that sets `spec` to its file sample.
fn file_line(spec: &Spec) -> String {
    let file = samples(spec.kind).file;
    match spec.kind {
        Kind::Port | Kind::Bool => format!("{} = {file}\n", spec.key),
        Kind::Path | Kind::Log | Kind::Ip | Kind::Domain => {
            format!("{} = \"{file}\"\n", spec.key)
        }
        Kind::IpList | Kind::HostList => {
            let items: Vec<String> = file.split(',').map(|item| format!("\"{item}\"")).collect();
            format!("{} = [{}]\n", spec.key, items.join(", "))
        }
    }
}

/// What `--print-config` shows for the remote serving options when nothing sets them.
const REMOTE_DEFAULTS: &str = "remote = off (default)\n\
     public_hosts = none (default)\n\
     workspace_base_domain = subdomains of the first public host (default)\n\
     listen_interface = no network listener (default)\n\
     listen_port = 8443 (default)\n\
     allowed_proxies = none (default)\n\
     multi_user = off (default)\n";

struct Rig {
    dir: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn file(&self) -> PathBuf {
        self.dir.path().join("serve.toml")
    }

    /// Resolves with the given places set for `spec` only.
    fn resolve(
        &self,
        spec: &'static Spec,
        flag: bool,
        env: bool,
        file: bool,
    ) -> Result<Effective, ConfigError> {
        let s = samples(spec.kind);
        let mut flags = Flags::new();
        let mut vars: BTreeMap<&str, OsString> = BTreeMap::new();
        if flag {
            flags.insert(spec.key, spec.kind.parse_text(s.flag).unwrap());
        }
        if env {
            vars.insert(spec.env, OsString::from(s.env));
        }
        if file {
            std::fs::write(self.file(), file_line(spec)).unwrap();
            flags.insert(CONFIG_KEY, Value::Path(self.file()));
        }
        resolve(&flags, &|name| vars.get(name).cloned(), self.dir.path())
    }
}

fn expect(spec: &Spec, text: &str, dir: &Path, source: Source) -> (Value, Source) {
    let value = spec.kind.parse_text(text).unwrap();
    (absolute(value, dir), source)
}

#[test]
fn each_option_takes_the_flag_over_the_environment_over_the_file_over_the_default() {
    let rig = Rig::new();
    let dir = rig.dir.path();
    for spec in OPTIONS.iter().filter(|spec| spec.in_file) {
        let s = samples(spec.kind);
        let flag = expect(spec, s.flag, dir, Source::Flag(spec.flag()));
        let env = expect(spec, s.env, dir, Source::Environment(spec.env));
        let file = expect(spec, s.file, dir, Source::File(rig.file()));
        let key = spec.key;
        let got = |f, e, c| rig.resolve(spec, f, e, c).unwrap().get(key).cloned();
        // Every pair, both ways round where the order matters.
        assert_eq!(
            got(true, true, false),
            Some(flag.clone()),
            "{key}: flag over environment"
        );
        assert_eq!(
            got(true, false, true),
            Some(flag.clone()),
            "{key}: flag over file"
        );
        assert_eq!(
            got(false, true, true),
            Some(env.clone()),
            "{key}: environment over file"
        );
        assert_eq!(
            got(true, true, true),
            Some(flag.clone()),
            "{key}: all three"
        );
        assert_eq!(got(true, false, false), Some(flag), "{key}: flag alone");
        assert_eq!(
            got(false, true, false),
            Some(env),
            "{key}: environment alone"
        );
        assert_eq!(got(false, false, true), Some(file), "{key}: file alone");
        assert_eq!(got(false, false, false), None, "{key}: default is no value");
    }
}

#[test]
fn the_config_file_is_named_by_the_flag_over_the_environment() {
    let rig = Rig::new();
    let flagged = rig.dir.path().join("flagged.toml");
    let named = rig.dir.path().join("named.toml");
    std::fs::write(&flagged, "port = 1\n").unwrap();
    std::fs::write(&named, "port = 2\n").unwrap();
    let env = |name: &str| (name == "PUDDLE_CONFIG").then(|| named.clone().into_os_string());
    let mut flags = Flags::new();
    let from_env = resolve(&flags, &env, rig.dir.path()).unwrap();
    assert_eq!(from_env.port(), Some(2));
    assert_eq!(
        from_env.get("config"),
        Some(&(
            Value::Path(named.clone()),
            Source::Environment("PUDDLE_CONFIG")
        ))
    );
    flags.insert("config", Value::Path(flagged.clone()));
    let from_flag = resolve(&flags, &env, rig.dir.path()).unwrap();
    assert_eq!(from_flag.port(), Some(1));
    assert_eq!(
        from_flag.get("config"),
        Some(&(Value::Path(flagged), Source::Flag("--config")))
    );
    assert_eq!(
        resolve(&Flags::new(), &|_| None, rig.dir.path())
            .unwrap()
            .get("config"),
        None
    );
}

#[test]
fn a_relative_path_from_a_flag_or_the_environment_means_the_working_directory() {
    let cwd = tempfile::tempdir().unwrap();
    let mut flags = Flags::new();
    flags.insert("data_dir", Value::Path(PathBuf::from("data")));
    let env = |name: &str| (name == "PUDDLE_CONNECTION_FILE").then(|| OsString::from("c/api.json"));
    let effective = resolve(&flags, &env, cwd.path()).unwrap();
    // An option that holds no path answers none.
    assert_eq!(effective.path("port"), None);
    assert_eq!(
        effective.path("data_dir"),
        Some(cwd.path().join("data").as_path())
    );
    assert_eq!(
        effective.path("connection_file"),
        Some(cwd.path().join("c/api.json").as_path())
    );
}

#[test]
fn an_invalid_environment_value_refuses_and_names_the_variable() {
    let rig = Rig::new();
    let cases = [
        ("PUDDLE_PORT", "http", "a port number"),
        ("PUDDLE_PORT", "70000", "a port number"),
        ("PUDDLE_LOG", "loud", "error, warn, info, debug or trace"),
        ("PUDDLE_DATA_DIR", "", "a non-empty path"),
    ];
    for (name, value, expected) in cases {
        let env = |n: &str| (n == name).then(|| OsString::from(value));
        let err = resolve(&Flags::new(), &env, rig.dir.path()).unwrap_err();
        let text = err.to_string();
        assert!(text.starts_with(name), "{text}");
        assert!(text.contains(&format!("{value:?}")), "{text}");
        assert!(text.contains(expected), "{text}");
    }
}

#[cfg(unix)]
#[test]
fn an_environment_value_that_is_not_unicode_refuses() {
    use std::os::unix::ffi::OsStringExt as _;
    let env = |n: &str| (n == "PUDDLE_LOG").then(|| OsString::from_vec(vec![0xff, 0xfe]));
    let err = resolve(&Flags::new(), &env, Path::new("/")).unwrap_err();
    assert!(err.to_string().contains("not valid Unicode"), "{err}");
}

#[test]
fn print_config_lists_every_option_with_its_value_and_where_it_came_from() {
    let rig = Rig::new();
    std::fs::write(rig.file(), "connection_file = \"api.json\"\nport = 9000\n").unwrap();
    let mut flags = Flags::new();
    flags.insert("config", Value::Path(rig.file()));
    flags.insert("port", Value::Port(4000));
    let env = |n: &str| (n == "PUDDLE_LOG").then(|| OsString::from("debug"));
    let out = resolve(&flags, &env, rig.dir.path()).unwrap().render();
    let file = rig.file();
    let expected = format!(
        "config = {file} (flag --config)\n\
         port = 4000 (flag --port)\n\
         connection_file = {api} (config file {file})\n\
         data_dir = the per-user data folder (default)\n\
         log = debug (environment PUDDLE_LOG)\n\
         {remote_defaults}",
        file = file.display(),
        api = rig.dir.path().join("api.json").display(),
        remote_defaults = REMOTE_DEFAULTS,
    );
    assert_eq!(out, expected);
}

#[test]
fn print_config_without_anything_set_shows_the_defaults() {
    assert_eq!(
        resolve(&Flags::new(), &|_| None, Path::new("/"))
            .unwrap()
            .port(),
        None
    );
    let out = resolve(&Flags::new(), &|_| None, Path::new("/"))
        .unwrap()
        .render();
    assert_eq!(
        out,
        format!(
            "config = no config file (default)\n\
             port = a free port (default)\n\
             connection_file = api.json in the data folder (default)\n\
             data_dir = the per-user data folder (default)\n\
             log = info (default)\n{REMOTE_DEFAULTS}"
        )
    );
}

#[test]
fn the_log_option_makes_the_filter_and_defaults_to_info() {
    let level = |filter: &ScopedFilter, target: &str, level: tracing::Level| {
        filter.would_enable(target, &level)
    };
    let default = resolve(&Flags::new(), &|_| None, Path::new("/"))
        .unwrap()
        .log_filter();
    assert!(level(&default, "puddle_host", tracing::Level::INFO));
    assert!(!level(&default, "puddle_host", tracing::Level::DEBUG));
    let env = |n: &str| (n == "PUDDLE_LOG").then(|| OsString::from("trace,hyper=debug"));
    let set = resolve(&Flags::new(), &env, Path::new("/"))
        .unwrap()
        .log_filter();
    assert!(level(&set, "puddle_host", tracing::Level::TRACE));
    assert!(level(&set, "hyper", tracing::Level::DEBUG));
    assert!(!level(&set, "hyper", tracing::Level::TRACE));
}

#[test]
fn the_table_has_unique_names_and_the_config_row_first() {
    let first = OPTIONS.first().unwrap();
    assert_eq!(first.key, CONFIG_KEY);
    assert!(!first.in_file, "the file cannot name itself");
    for (i, a) in OPTIONS.iter().enumerate() {
        assert!(a.usage.starts_with("--"), "{}", a.usage);
        assert_eq!(
            a.env,
            format!("PUDDLE_{}", a.key.to_uppercase()),
            "{}",
            a.key
        );
        assert_eq!(spec_for_key(a.key).map(|s| s.key), Some(a.key));
        assert_eq!(spec_for_flag(a.flag()).map(|s| s.key), Some(a.key));
        for b in OPTIONS.iter().skip(i + 1) {
            assert_ne!(a.key, b.key);
            assert_ne!(a.flag(), b.flag());
            assert_ne!(a.env, b.env);
        }
    }
    assert!(spec_for_key("nope").is_none());
    assert!(spec_for_flag("--nope").is_none());
}

#[test]
fn the_usage_text_and_the_docs_name_every_option() {
    let docs = include_str!("../../../../../../docs/serve.md");
    assert!(crate::cli::USAGE.contains(crate::cmd::serve::PRINT_CONFIG_FLAG));
    assert!(docs.contains(crate::cmd::serve::PRINT_CONFIG_FLAG));
    for spec in &OPTIONS {
        assert!(
            crate::cli::USAGE.contains(spec.usage),
            "usage lacks {}",
            spec.usage
        );
        assert!(docs.contains(spec.usage), "docs lack {}", spec.usage);
        assert!(docs.contains(spec.env), "docs lack {}", spec.env);
        assert!(
            docs.contains(spec.default),
            "docs lack the default of {}",
            spec.key
        );
        if spec.in_file {
            assert!(
                docs.contains(&format!("`{}`", spec.key)),
                "docs lack {}",
                spec.key
            );
        }
    }
}

#[test]
fn the_example_file_in_the_docs_is_accepted() {
    let docs = include_str!("../../../../../../docs/serve.md");
    let example = docs
        .split("```toml\n")
        .nth(1)
        .and_then(|rest| rest.split("```").next())
        .expect("the docs hold a toml example");
    let dir = tempfile::tempdir().unwrap();
    let values = config_file::parse(example, &dir.path().join("serve.toml")).unwrap();
    for spec in OPTIONS.iter().filter(|spec| spec.in_file) {
        assert!(
            values.get(spec.key).is_some(),
            "the example lacks {}",
            spec.key
        );
    }
}

#[test]
fn the_flags_and_the_file_keys_mirror_each_other() {
    for spec in &OPTIONS {
        // Same name: kebab-case on the line, snake_case in the file.
        assert_eq!(spec.flag(), format!("--{}", spec.key.replace('_', "-")));
        // The same check and message for a value from a flag and from the file.
        assert_eq!(spec_for_flag(spec.flag()).map(|s| s.kind), Some(spec.kind));
    }
    // Every file key has a flag, and every flag has a file key but `--config`.
    let file_keys: Vec<_> = OPTIONS
        .iter()
        .filter(|s| s.in_file)
        .map(|s| s.key)
        .collect();
    let flag_keys: Vec<_> = OPTIONS
        .iter()
        .filter(|s| spec_for_flag(s.flag()).is_some() && s.key != CONFIG_KEY)
        .map(|s| s.key)
        .collect();
    assert_eq!(file_keys, flag_keys);
    assert_eq!(
        OPTIONS
            .iter()
            .filter(|s| !s.in_file)
            .map(|s| s.key)
            .collect::<Vec<_>>(),
        [CONFIG_KEY]
    );
}

#[test]
fn a_switch_is_flag_and_negated_flag_in_the_table_the_usage_and_the_docs() {
    let docs = include_str!("../../../../../../docs/serve.md");
    let switches: Vec<_> = OPTIONS.iter().filter(|s| s.kind.is_switch()).collect();
    assert_eq!(switches.len(), 2);
    for spec in switches {
        let negated = spec.negated_flag().unwrap();
        assert_eq!(negated, format!("--no-{}", spec.key.replace('_', "-")));
        assert_eq!(
            spec_for_negated_flag(&negated).map(|s| s.key),
            Some(spec.key)
        );
        assert!(
            crate::cli::USAGE.contains(&negated),
            "usage lacks {negated}"
        );
        assert!(docs.contains(&negated), "docs lack {negated}");
        assert!(!spec.usage.contains(' '), "a switch takes no value");
    }
    assert!(spec_for_negated_flag("--no-port").is_none());
    assert!(spec_for_negated_flag("--remote").is_none());
    assert_eq!(spec_for_key("port").unwrap().negated_flag(), None);
}

#[test]
fn each_kind_names_what_it_expects_and_refuses_the_rest() {
    let bad = [
        (Kind::Bool, "yes"),
        (Kind::Bool, ""),
        (Kind::Ip, "example.org"),
        (Kind::Ip, "10.0.0"),
        (Kind::IpList, "10.0.0.1,"),
        (Kind::IpList, "10.0.0.1,nope"),
        (Kind::HostList, ""),
        (Kind::HostList, "https://a.example.org"),
        (Kind::HostList, "a.example.org/path"),
        (Kind::HostList, "a.example.org:0"),
        (Kind::HostList, "a.example.org:99999"),
        (Kind::HostList, "a_b.example.org"),
        (Kind::HostList, "a.example.org,,b.example.org"),
        (Kind::Domain, "not a domain"),
        (Kind::Domain, ""),
        (Kind::Domain, "a..example.org"),
    ];
    for (kind, text) in bad {
        assert_eq!(
            kind.parse_text(text),
            Err(kind.expects()),
            "{kind:?} {text:?}"
        );
    }
    assert_eq!(Kind::Bool.parse_text("true"), Ok(Value::Bool(true)));
    assert_eq!(Kind::Bool.parse_text("false"), Ok(Value::Bool(false)));
    assert_eq!(
        Kind::Ip.parse_text("2001:db8::1"),
        Ok(Value::Ip("2001:db8::1".parse().unwrap()))
    );
    assert_eq!(
        Kind::IpList.parse_text(" 10.0.0.1 , ::1 "),
        Ok(Value::IpList(vec![
            "10.0.0.1".parse().unwrap(),
            "::1".parse().unwrap()
        ]))
    );
    // Case is folded, `:443` is the same as none, and a bracketed IPv6 address is a host.
    assert_eq!(
        Kind::HostList.parse_text("Puddle.Example.ORG:443,a.example.org.,[::1]:8443"),
        Ok(Value::Hosts(vec![
            "puddle.example.org".into(),
            "a.example.org".into(),
            "[::1]:8443".into()
        ]))
    );
    assert_eq!(
        Kind::Domain.parse_text("WS.Example.Net."),
        Ok(Value::Domain("ws.example.net".into()))
    );
    assert!(Kind::HostList.is_list() && Kind::IpList.is_list());
    assert!(!Kind::Bool.is_list() && !Kind::Domain.is_list());
}

#[test]
fn a_switch_in_the_environment_is_true_or_false_and_nothing_else() {
    let set = |value: &'static str| {
        let env = move |n: &str| (n == "PUDDLE_REMOTE").then(|| OsString::from(value));
        resolve(&Flags::new(), &env, Path::new("/"))
    };
    assert_eq!(set("true").unwrap().switch("remote"), Some(true));
    assert_eq!(set("false").unwrap().switch("remote"), Some(false));
    let err = set("1").unwrap_err().to_string();
    assert!(
        err.starts_with("PUDDLE_REMOTE has the invalid value \"1\""),
        "{err}"
    );
    assert!(err.contains("true or false"), "{err}");
    assert_eq!(
        resolve(&Flags::new(), &|_| None, Path::new("/"))
            .unwrap()
            .switch("remote"),
        None
    );
}

/// The options for these environment values.
fn with_env(vars: &[(&'static str, &'static str)]) -> Effective {
    let vars: BTreeMap<&str, &str> = vars.iter().copied().collect();
    resolve(
        &Flags::new(),
        &|name| vars.get(name).map(OsString::from),
        Path::new("/"),
    )
    .unwrap()
}

#[test]
fn remote_serving_off_looks_at_no_other_remote_option() {
    assert_eq!(with_env(&[]).remote(), Ok(None));
    assert_eq!(with_env(&[("PUDDLE_REMOTE", "false")]).remote(), Ok(None));
    // `--no-remote` over a file that sets everything switches it all off.
    assert_eq!(
        with_env(&[
            ("PUDDLE_REMOTE", "false"),
            ("PUDDLE_LISTEN_PORT", "9000"),
            ("PUDDLE_ALLOWED_PROXIES", "10.0.0.1"),
        ])
        .remote(),
        Ok(None)
    );
    assert_eq!(
        with_env(&[("PUDDLE_PUBLIC_HOSTS", "puddle.example.org")]).remote(),
        Ok(None)
    );
}

#[test]
fn remote_serving_on_needs_a_public_host() {
    let err = with_env(&[("PUDDLE_REMOTE", "true")]).remote().unwrap_err();
    assert_eq!(
        err.to_string(),
        "remote serving: remote serving needs at least one public host (`public_hosts`)"
    );
}

#[test]
fn remote_serving_on_takes_the_public_hosts_and_defaults_the_workspace_names_to_subdomains() {
    let remote = with_env(&[
        ("PUDDLE_REMOTE", "true"),
        (
            "PUDDLE_PUBLIC_HOSTS",
            "puddle.example.org,alt.example.org:8443",
        ),
    ])
    .remote()
    .unwrap()
    .unwrap();
    let hosts: Vec<String> = remote
        .serving
        .public_hosts()
        .iter()
        .map(PublicHost::authority)
        .collect();
    assert_eq!(hosts, ["puddle.example.org", "alt.example.org:8443"]);
    assert_eq!(
        remote.serving.workspace_names().base_domain(),
        "puddle.example.org"
    );
    assert_eq!(remote.network, None);
    let own = with_env(&[
        ("PUDDLE_REMOTE", "true"),
        ("PUDDLE_PUBLIC_HOSTS", "puddle.example.org"),
        ("PUDDLE_WORKSPACE_BASE_DOMAIN", "ws.example.net"),
    ])
    .remote()
    .unwrap()
    .unwrap();
    assert_eq!(
        own.serving.workspace_names().base_domain(),
        "ws.example.net"
    );
}

#[test]
fn the_network_listener_needs_an_interface_and_a_proxy_and_defaults_its_port() {
    let base = [
        ("PUDDLE_REMOTE", "true"),
        ("PUDDLE_PUBLIC_HOSTS", "puddle.example.org"),
    ];
    let with = |extra: &[(&'static str, &'static str)]| {
        let mut vars = base.to_vec();
        vars.extend_from_slice(extra);
        with_env(&vars).remote()
    };
    let listener = with(&[
        ("PUDDLE_LISTEN_INTERFACE", "192.168.1.10"),
        ("PUDDLE_ALLOWED_PROXIES", "192.168.1.20,192.168.1.21"),
    ])
    .unwrap()
    .unwrap()
    .network
    .unwrap();
    assert_eq!(
        listener.interface(),
        "192.168.1.10".parse::<IpAddr>().unwrap()
    );
    assert_eq!(listener.port(), DEFAULT_LISTEN_PORT);
    assert_eq!(listener.allowed_proxies().len(), 2);
    let set_port = with(&[
        ("PUDDLE_LISTEN_INTERFACE", "192.168.1.10"),
        ("PUDDLE_LISTEN_PORT", "9443"),
        ("PUDDLE_ALLOWED_PROXIES", "192.168.1.20"),
    ])
    .unwrap()
    .unwrap();
    assert_eq!(set_port.network.unwrap().port(), 9443);
    for (interface, proxies, said) in [
        ("0.0.0.0", "192.168.1.20", "unspecified address"),
        ("::", "192.168.1.20", "unspecified address"),
        ("192.168.1.10", "192.168.1.20,0.0.0.0", "allowed proxy"),
    ] {
        let err = with(&[
            ("PUDDLE_LISTEN_INTERFACE", interface),
            ("PUDDLE_ALLOWED_PROXIES", proxies),
        ])
        .unwrap_err();
        assert!(
            err.to_string().contains(said),
            "{interface} {proxies}: {err}"
        );
    }
    let clash = with_env(&[
        ("PUDDLE_REMOTE", "true"),
        (
            "PUDDLE_PUBLIC_HOSTS",
            "puddle.example.org,box.ws.example.net",
        ),
        ("PUDDLE_WORKSPACE_BASE_DOMAIN", "ws.example.net"),
    ])
    .remote()
    .unwrap_err();
    assert!(clash.to_string().contains("one label below"), "{clash}");
    let none_allowed = with(&[("PUDDLE_LISTEN_INTERFACE", "192.168.1.10")]).unwrap_err();
    assert!(
        none_allowed.to_string().contains("`allowed_proxies`"),
        "{none_allowed}"
    );
    for orphan in [
        ("PUDDLE_LISTEN_PORT", "9443"),
        ("PUDDLE_ALLOWED_PROXIES", "10.0.0.1"),
    ] {
        let err = with(&[orphan]).unwrap_err().to_string();
        assert!(err.contains("set `listen_interface` too"), "{err}");
    }
}

#[test]
fn remote_serving_refuses_a_base_domain_that_cannot_tell_workspaces_from_the_ui() {
    let err =
        |vars: &[(&'static str, &'static str)]| with_env(vars).remote().unwrap_err().to_string();
    assert!(
        err(&[
            ("PUDDLE_REMOTE", "true"),
            ("PUDDLE_PUBLIC_HOSTS", "puddle.example.org"),
            ("PUDDLE_WORKSPACE_BASE_DOMAIN", "puddle.example.org"),
        ])
        .contains("is also a public host")
    );
    assert!(
        err(&[
            ("PUDDLE_REMOTE", "true"),
            ("PUDDLE_PUBLIC_HOSTS", "203.0.113.7")
        ])
        .contains("set `workspace_base_domain`")
    );
}

#[test]
fn a_negated_switch_is_reported_as_the_flag_that_was_typed() {
    let mut flags = Flags::new();
    flags.insert("remote", Value::Bool(false));
    let off = resolve(&flags, &|_| None, Path::new("/")).unwrap();
    assert_eq!(
        off.get("remote"),
        Some(&(Value::Bool(false), Source::NegatedFlag("--remote")))
    );
    assert!(off.render().contains("remote = false (flag --no-remote)\n"));
    flags.insert("remote", Value::Bool(true));
    let on = resolve(&flags, &|_| None, Path::new("/")).unwrap();
    assert!(on.render().contains("remote = true (flag --remote)\n"));
}

#[test]
fn only_a_list_kind_is_made_of_items() {
    assert_eq!(Kind::Port.parse_items(&["1"]), Err(Kind::Port.expects()));
    assert_eq!(Kind::IpList.parse_items(&[]), Err(Kind::IpList.expects()));
}
