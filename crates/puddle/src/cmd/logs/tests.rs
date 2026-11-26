// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::cmd::serve::options::Value;

// Assembled at compile time so the secret scan finds no token-shaped literal in the tree.
const TOKEN: &str = concat!("gh", "p_aB3dE6gH9jK2mN5pQ8sT1vW4yZ7bC0eF3hJ6");

#[test]
fn bundle_with_and_without_options() {
    assert_eq!(
        parse(["bundle"].into_iter()),
        Ok(Command::Logs(LogsArgs::default()))
    );
    let parsed = parse(["bundle", "--output", "out.tar.gz", "--data-dir", "d"].into_iter());
    let Ok(Command::Logs(args)) = parsed else {
        panic!("{parsed:?}");
    };
    assert_eq!(args.output, Some("out.tar.gz".into()));
    assert_eq!(args.flags.get("data_dir"), Some(&Value::Path("d".into())));
}

#[test]
fn anything_else_is_a_usage_error() {
    assert_eq!(
        parse(Vec::<&str>::new().into_iter()),
        Err(UsageError::MissingArgument("logs bundle"))
    );
    assert_eq!(
        parse(["show"].into_iter()),
        Err(UsageError::UnknownArgument("show".into()))
    );
    assert_eq!(
        parse(["bundle", "--output"].into_iter()),
        Err(UsageError::MissingArgument("--output <path>"))
    );
    assert_eq!(
        parse(["bundle", "--output", "a", "--output", "b"].into_iter()),
        Err(UsageError::UnknownArgument("--output".into()))
    );
}

#[test]
fn the_command_writes_a_bundle_with_the_secret_cut_out_of_the_settings_and_the_log() {
    let data = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(data.path().join("settings")).unwrap();
    std::fs::write(
        data.path().join("settings/global.json"),
        format!(r#"{{"schema_version":2,"ui":{{"log_level":"debug"}},"api_token":"{TOKEN}"}}"#),
    )
    .unwrap();
    let logs = puddle_log::log_folder(data.path());
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(
        logs.join(puddle_log::FILE_NAME),
        format!("proxy token={TOKEN} host=github.com\n"),
    )
    .unwrap();
    let out = data.path().join("bundle.tar.gz");
    let mut flags = Flags::new();
    flags.insert("data_dir", Value::Path(data.path().into()));
    let written = make(&LogsArgs {
        output: Some(out.clone()),
        flags,
    })
    .unwrap();
    assert_eq!(written, out);

    let bytes = std::fs::read(&out).unwrap();
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes.as_slice()));
    let mut all = String::new();
    for entry in archive.entries().unwrap() {
        std::io::Read::read_to_string(&mut entry.unwrap(), &mut all).unwrap();
    }
    assert!(!all.contains("ghp_"), "a token is in the bundle:\n{all}");
    assert!(
        all.contains("host=github.com") && all.contains("log level debug"),
        "{all}"
    );
}
