// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn accepts_ci_style_prefixes() {
    for ok in ["r123456789-1", "v0abc", "a", "host-3"] {
        assert_eq!(RunPrefix::new(ok).unwrap().as_str(), ok);
    }
}

#[test]
fn rejects_prefixes_that_break_names_or_paths() {
    for bad in [
        "",
        "1abc",
        "-abc",
        "abc-",
        "ABC",
        "a_b",
        "a.b",
        "a/b",
        "a b",
        "abcdefghijklmnopqrstu",
    ] {
        let err = RunPrefix::new(bad).unwrap_err();
        assert!(
            matches!(err, HarnessError::InvalidPrefix { max: 20, .. }),
            "{bad:?} gave {err}"
        );
    }
}

#[test]
fn generated_prefixes_are_valid_and_differ_by_seed() {
    let generated = RunPrefix::generate();
    assert_eq!(RunPrefix::new(generated.as_str()).unwrap(), generated);
    assert_eq!(generated.as_str().len(), 9);
    assert_ne!(RunPrefix::from_seed(1), RunPrefix::from_seed(2));
    assert_eq!(RunPrefix::from_seed(0).as_str(), "v00000000");
    assert_eq!(RunPrefix::from_seed(u128::MAX).to_string().len(), 9);
}

#[test]
fn value_wins_over_generation() {
    let given = RunPrefix::from_value_or_generate(Some("r42-1")).unwrap();
    assert_eq!(given.as_str(), "r42-1");
    assert!(RunPrefix::from_value_or_generate(Some("Bad")).is_err());
    let generated = RunPrefix::from_value_or_generate(None).unwrap();
    assert!(generated.as_str().starts_with('v'));
}

#[test]
fn sandbox_names_are_prefixed_and_validated() {
    let prefix = RunPrefix::new("r42-1").unwrap();
    assert_eq!(
        prefix.sandbox_name("smoke").unwrap().as_str(),
        "r42-1-smoke"
    );
    let err = prefix.sandbox_name("Not_Valid").unwrap_err();
    assert!(matches!(err, HarnessError::InvalidName { .. }), "{err}");
    assert!(prefix.sandbox_name(&"x".repeat(80)).is_err());
}

#[test]
fn owns_only_its_own_names() {
    let prefix = RunPrefix::new("r42-1").unwrap();
    assert!(prefix.owns("r42-1-smoke"));
    assert!(!prefix.owns("r42-1"));
    assert!(!prefix.owns("r42-10-smoke"));
    assert!(!prefix.owns("other-smoke"));
}
