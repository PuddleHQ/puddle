// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

fn check(text: &str) -> Result<FileValues, ConfigError> {
    let dir = tempfile::tempdir().unwrap();
    parse(text, &dir.path().join("serve.toml"))
}

fn message(text: &str) -> String {
    check(text).unwrap_err().to_string()
}

#[test]
fn a_file_may_set_every_key_and_nothing_else() {
    let values = check(
        "port = 8080\nconnection_file = \"/run/api.json\"\ndata_dir = \"/srv\"\nlog = \"warn\"\n",
    )
    .unwrap();
    assert_eq!(values.get("port"), Some(&Value::Port(8080)));
    assert_eq!(values.get("log"), Some(&Value::Log("warn".to_owned())));
    assert_eq!(check("").unwrap(), FileValues::default());
    assert_eq!(check("# only a comment\n").unwrap(), FileValues::default());
}

#[test]
fn a_relative_path_means_the_folder_of_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("conf").join("serve.toml");
    let values = parse(
        "connection_file = \"api.json\"\ndata_dir = \"../data\"\n",
        &file,
    )
    .unwrap();
    assert_eq!(
        values.get("connection_file"),
        Some(&Value::Path(dir.path().join("conf").join("api.json")))
    );
    assert_eq!(
        values.get("data_dir"),
        Some(&Value::Path(dir.path().join("conf").join("../data")))
    );
    let absolute = dir.path().join("elsewhere");
    let text = format!("data_dir = {:?}\n", absolute.to_str().unwrap());
    let values = parse(&text, &file).unwrap();
    assert_eq!(values.get("data_dir"), Some(&Value::Path(absolute)));
}

#[test]
fn an_unknown_key_names_the_file_the_line_and_the_keys() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("serve.toml");
    let err = parse("port = 1\n\nprot = 2\n", &file).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "{}:3: unknown key `prot`; the keys are port, connection_file, data_dir, log; fix the spelling or remove the line",
            file.display()
        )
    );
}

#[test]
fn the_config_option_is_no_key_of_the_file() {
    let text = message("config = \"other.toml\"\n");
    assert!(text.contains("unknown key `config`"), "{text}");
}

#[test]
fn a_table_is_not_a_key() {
    let text = message("[server]\nport = 1\n");
    assert!(text.contains(":1: unknown key `server`"), "{text}");
}

#[test]
fn the_first_problem_in_file_order_is_reported() {
    let text = message("port = 1\nzzz = 1\naaa = 2\n");
    assert!(text.contains(":2: unknown key `zzz`"), "{text}");
}

#[test]
fn a_wrong_type_names_the_line_what_the_key_takes_and_how_to_write_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("serve.toml");
    let err = parse("log = \"info\"\nport = \"8080\"\n", &file).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "{}:2: `port` must be a port number from 0 to 65535, found a string; write it as `port = 8080`",
            file.display()
        )
    );
    let text = message("data_dir = 5\n");
    assert!(
        text.contains("`data_dir` must be a non-empty path, found an integer"),
        "{text}"
    );
    assert!(
        text.contains("write it as `data_dir = \"/srv/puddle\"`"),
        "{text}"
    );
    assert!(message("port = true\n").contains("found a boolean"));
    assert!(message("port = 1.5\n").contains("found a float"));
    assert!(message("log = [\"info\"]\n").contains("found an array"));
}

#[test]
fn a_value_out_of_range_or_empty_or_not_a_filter_is_refused() {
    assert!(message("port = 70000\n").contains("found the integer 70000"));
    assert!(message("port = -1\n").contains("found the integer -1"));
    assert!(message("connection_file = \"\"\n").contains("found an empty string"));
    let text = message("log = \"loud\"\n");
    assert!(
        text.contains("`log` must be error, warn, info, debug or trace"),
        "{text}"
    );
    assert!(text.contains("found the string \"loud\""), "{text}");
    assert_eq!(
        check("port = 0\n").unwrap().get("port"),
        Some(&Value::Port(0))
    );
    assert_eq!(
        check("port = 65_535\n").unwrap().get("port"),
        Some(&Value::Port(65535))
    );
    assert_eq!(
        check("port = 0x1F90\n").unwrap().get("port"),
        Some(&Value::Port(8080))
    );
}

#[test]
fn a_repeated_key_refuses_with_the_line_of_the_second() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("serve.toml");
    let err = parse("port = 1\nlog = \"info\"\nport = 2\n", &file).unwrap_err();
    let text = err.to_string();
    assert!(
        text.starts_with(&format!("{}:3:1: ", file.display())),
        "{text}"
    );
    assert!(text.contains("duplicate key"), "{text}");
    assert!(
        text.ends_with("a key may appear once: remove one of the lines"),
        "{text}"
    );
}

#[test]
fn a_syntax_error_names_line_and_column() {
    let text = message("port = 1\nlog = \n");
    assert!(text.contains(":2:"), "{text}");
    assert!(text.ends_with("fix the TOML on that line"), "{text}");
    let text = message("port == 1\n");
    assert!(text.contains(":1:"), "{text}");
}

#[test]
fn a_missing_file_refuses_and_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("absent.toml");
    let text = load(&file).unwrap_err().to_string();
    assert!(
        text.starts_with(&format!("cannot read the config file {}", file.display())),
        "{text}"
    );
    assert!(text.contains("check the path"), "{text}");
    // A folder is no file either.
    assert!(load(dir.path()).is_err());
}

#[test]
fn a_file_that_is_not_utf8_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("serve.toml");
    std::fs::write(&file, [0xff, 0xfe, 0x00]).unwrap();
    let text = load(&file).unwrap_err().to_string();
    assert!(text.contains("cannot read the config file"), "{text}");
}

#[test]
fn load_reads_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("serve.toml");
    std::fs::write(&file, "port = 7\n").unwrap();
    assert_eq!(load(&file).unwrap().get("port"), Some(&Value::Port(7)));
}

#[test]
fn positions_count_lines_and_columns_from_one() {
    assert_eq!(position("ab\ncd", 0), (1, 1));
    assert_eq!(position("ab\ncd", 4), (2, 2));
    assert_eq!(position("ab\ncd", 99), (2, 3));
    assert_eq!(position("é=1", 3), (1, 3));
}
