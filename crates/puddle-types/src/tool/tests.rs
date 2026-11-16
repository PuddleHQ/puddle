// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn names_and_versions_take_what_mise_writes() {
    for name in [
        "node",
        "npm:prettier",
        "github:cli/cli",
        "npm:@a/b",
        "a.b",
        "x-1_2",
    ] {
        assert_eq!(ToolName::new(name).unwrap().as_str(), name);
    }
    for version in [
        "22",
        "22.11.0",
        "lts",
        "lts/iron",
        "prefix:1.2",
        "1.0.0+build",
    ] {
        assert_eq!(ToolVersion::new(version).unwrap().to_string(), version);
    }
    assert!(ToolName::new(&"a".repeat(ToolName::MAX_LEN)).is_ok());
    assert_eq!("go".parse::<ToolName>().unwrap().as_str(), "go");
    assert_eq!(String::from(ToolVersion::new("1").unwrap()), "1");
}

#[test]
fn what_could_break_out_of_a_toml_key_or_read_as_an_option_is_refused() {
    let long = "a".repeat(ToolName::MAX_LEN + 1);
    for bad in [
        "", "-x", ".x", "a b", "a\"b", "a\\b", "a\nb", "a=b", "é", "a]", &long,
    ] {
        assert!(ToolName::new(bad).is_err(), "{bad:?}");
        assert!(ToolVersion::new(bad).is_err(), "{bad:?}");
    }
    assert!(ToolVersion::new("1+").is_ok());
    assert!(ToolName::new("a+").is_err());
    assert!(serde_json::from_str::<ToolName>("\"a b\"").is_err());
    assert!(ToolName::try_from("a b".to_owned()).is_err());
    assert_eq!(
        ToolName::new("-x").unwrap_err().reason(),
        "must start with a letter or digit"
    );
}

#[test]
fn a_file_hash_is_sha256_hex_or_absent() {
    let hex = "0123456789abcdef".repeat(4);
    assert_eq!(ToolsFileHash::new(&hex).unwrap().as_str(), hex);
    assert_eq!(ToolsFileHash::absent().to_string(), "absent");
    assert_eq!(
        "absent".parse::<ToolsFileHash>().unwrap(),
        ToolsFileHash::absent()
    );
    for bad in [
        "",
        "absent ",
        &hex.to_uppercase(),
        &hex[1..],
        "g".repeat(64).as_str(),
    ] {
        assert!(ToolsFileHash::new(bad).is_err(), "{bad:?}");
    }
    assert_eq!(
        serde_json::to_string(&ToolsFileHash::absent()).unwrap(),
        "\"absent\""
    );
    assert_eq!(
        String::from(ToolsFileHash::try_from(hex.clone()).unwrap()),
        hex
    );
}
