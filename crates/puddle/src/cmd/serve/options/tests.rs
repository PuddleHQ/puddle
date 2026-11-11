// SPDX-License-Identifier: GPL-3.0-or-later
use std::collections::BTreeMap;

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
    }
}

/// The config file line that sets `spec` to its file sample.
fn file_line(spec: &Spec) -> String {
    let file = samples(spec.kind).file;
    match spec.kind {
        Kind::Port => format!("{} = {file}\n", spec.key),
        Kind::Path | Kind::Log => format!("{} = \"{file}\"\n", spec.key),
    }
}

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
         log = debug (environment PUDDLE_LOG)\n",
        file = file.display(),
        api = rig.dir.path().join("api.json").display(),
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
        "config = no config file (default)\n\
         port = a free port (default)\n\
         connection_file = api.json in the data folder (default)\n\
         data_dir = the per-user data folder (default)\n\
         log = info (default)\n"
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
