// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

/// A document whose serialisation always fails, as a map with a non-string key would.
#[derive(serde::Deserialize)]
struct Unserialisable;

impl Serialize for Unserialisable {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("cannot be written"))
    }
}

impl Document for Unserialisable {
    const KIND: &'static str = "test";
    const VERSION: u32 = 1;
    const MIGRATIONS: &'static [Migration] = &[];

    fn collect_unknown(&self, _: &mut Vec<String>) {}
}

#[test]
fn settings_that_cannot_be_serialised_are_an_error_naming_the_kind() {
    let err = store(&Unserialisable).unwrap_err();
    assert!(
        matches!(&err, SettingsError::Unstorable { kind: "test", reason } if reason.contains("cannot be written")),
        "{err}"
    );
}
