// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn canon(path: &str) -> String {
    CanonicalPath::new(path).segments().join("/")
}

#[test]
fn every_spelling_of_one_path_is_one_path() {
    for spelling in [
        "/admin",
        "/ADMIN",
        "/Admin/",
        "//admin",
        "///admin//",
        "/./admin",
        "/x/../admin",
        "/x/y/../../admin",
        "/%61dmin",
        "/%41DMIN",
        "/a%64min",
        "/%2fadmin",
        "/%2f%2fadmin",
        "/x/%2e%2e/admin",
        "/x/%2E%2E/admin",
        "/x/.%2e/admin",
        "/x/%2e./admin",
        "/x%2f..%2fadmin",
        "/x%5c..%5cadmin",
        "/x\\..\\admin",
        "/%252e%252e/admin",
        "/x/%252e%252e/admin",
        "/%25%32%65%25%32%65/admin",
        "/admin;x=1",
        "/admin;x=1/",
        "/;x/admin",
        "/admin.",
        "/admin..",
        "/admin%20",
        "/admin%2e",
        "/admin%00",
        "/admin%00.png",
        "/admin%u0000x",
        "/%u0061dmin",
        "/adm%0ain",
        "/adm\tin",
        "/../admin",
        "/../../../admin",
        "/..%2f..%2fadmin",
        "/./././admin",
        "/x/.. /admin",
        "/x/../ /admin",
    ] {
        assert_eq!(canon(spelling), "admin", "{spelling:?}");
    }
}

#[test]
fn different_paths_stay_different() {
    assert_eq!(canon("/"), "");
    assert_eq!(canon(""), "");
    assert_eq!(canon("/administrator"), "administrator");
    assert_eq!(canon("/a/b"), "a/b");
    assert_eq!(canon("/a/.b"), "a/.b");
    assert_eq!(canon("/v1.0/x"), "v1.0/x");
    assert_eq!(canon("/%zzadmin"), "%zzadmin");
    assert_eq!(canon("/%"), "%");
    assert_eq!(canon("/%4"), "%4");
    assert_eq!(canon("/%u12"), "%u12");
}

#[test]
fn a_path_that_never_stops_decoding_ends_in_bounded_work() {
    let deep = "%25".repeat(200);
    assert_ne!(canon(&format!("/{deep}41")), "");
    assert!(canon("/%2525252525252525252e%2525252525252525252e/x").contains('%'));
}

#[test]
fn a_path_of_the_wrong_bytes_is_still_a_path() {
    assert_eq!(canon("/%ff%fe/x"), "\u{fffd}\u{fffd}/x");
    assert_eq!(canon("/%c3%28"), "\u{fffd}(");
    assert_eq!(canon("/\u{e9}"), "\u{e9}");
    assert_eq!(canon("/%C3%89"), "\u{e9}");
}

#[test]
fn a_pattern_is_kept_in_canonical_form() {
    for (text, kept) in [
        ("/", "/"),
        ("/**", "/**"),
        ("/gists", "/gists"),
        ("/Gists/", "/gists"),
        ("//gists//", "/gists"),
        ("/%67ists", "/gists"),
        ("/a/../gists", "/gists"),
        ("/repos/*/issues", "/repos/*/issues"),
        ("/repos/*/issues/**", "/repos/*/issues/**"),
    ] {
        assert_eq!(
            PathPattern::parse(text).unwrap().to_string(),
            kept,
            "{text}"
        );
    }
    // an encoded star is not a wildcard, and a segment cannot be one
    assert_eq!(
        PathPattern::parse("/repos/%2A/x"),
        Err(PathPatternError::PartialWildcard)
    );
}

#[test]
fn a_pattern_that_is_not_a_path_is_refused() {
    assert_eq!(PathPattern::parse(""), Err(PathPatternError::Shape));
    assert_eq!(PathPattern::parse("gists"), Err(PathPatternError::Shape));
    assert_eq!(
        PathPattern::parse(&format!("/{}", "a".repeat(MAX_PATH_PATTERN))),
        Err(PathPatternError::Shape)
    );
    assert_eq!(PathPattern::parse("/a?b"), Err(PathPatternError::Query));
    assert_eq!(PathPattern::parse("/a#b"), Err(PathPatternError::Query));
    assert_eq!(
        PathPattern::parse("/a*"),
        Err(PathPatternError::PartialWildcard)
    );
    assert_eq!(
        PathPattern::parse("/*a"),
        Err(PathPatternError::PartialWildcard)
    );
    assert_eq!(
        PathPattern::parse("/***"),
        Err(PathPatternError::PartialWildcard)
    );
    assert_eq!(
        PathPattern::parse("/**/a"),
        Err(PathPatternError::OpenNotLast)
    );
    assert_eq!(
        PathPattern::parse("/**/**"),
        Err(PathPatternError::OpenNotLast)
    );
    let long = "/a".repeat(MAX_PATH_SEGMENTS + 1);
    assert_eq!(PathPattern::parse(&long), Err(PathPatternError::TooLong));
    assert!(PathPattern::parse(&"/a".repeat(MAX_PATH_SEGMENTS)).is_ok());
    // stand-in characters, control characters, and wildcards that decoding would make or
    // remove are refused
    for bad in [
        "/\u{e000}",
        "/\u{e001}",
        "/a\u{1}",
        "/\n",
        "/*/..",
        "/%u0001one",
    ] {
        let parsed = PathPattern::parse(bad);
        assert!(
            parsed.is_err() || !parsed.unwrap().matches(&CanonicalPath::new("/x")),
            "{bad:?}"
        );
    }
    assert_eq!(
        PathPattern::parse("/\u{e000}"),
        Err(PathPatternError::Shape)
    );
    assert_eq!(PathPattern::parse("/*/.."), Err(PathPatternError::Shape));
}

fn pattern_matches(pattern: &str, path: &str) -> bool {
    PathPattern::parse(pattern)
        .unwrap()
        .matches(&CanonicalPath::new(path))
}

#[test]
fn a_pattern_without_wildcards_names_that_path_alone() {
    assert!(pattern_matches("/gists", "/gists"));
    assert!(pattern_matches("/gists", "/gists/"));
    assert!(pattern_matches("/gists", "//GISTS"));
    assert!(!pattern_matches("/gists", "/gists/1"));
    assert!(!pattern_matches("/gists", "/"));
    assert!(!pattern_matches("/gists", "/gist"));
    assert!(pattern_matches("/", "/"));
    assert!(pattern_matches("/", "//"));
    assert!(pattern_matches("/", "/x/.."));
    assert!(!pattern_matches("/", "/x"));
}

#[test]
fn a_wildcard_takes_one_segment_and_a_closing_one_takes_the_rest() {
    assert!(pattern_matches("/repos/*/issues", "/repos/me/issues"));
    assert!(!pattern_matches("/repos/*/issues", "/repos/issues"));
    assert!(!pattern_matches("/repos/*/issues", "/repos/a/b/issues"));
    assert!(!pattern_matches("/repos/*/issues", "/repos/me/issues/1"));
    assert!(pattern_matches("/gists/**", "/gists"));
    assert!(pattern_matches("/gists/**", "/gists/1/comments"));
    assert!(!pattern_matches("/gists/**", "/gist"));
    assert!(pattern_matches("/**", "/"));
    assert!(pattern_matches("/**", "/a/b/c"));
    assert!(PathPattern::any().matches(&CanonicalPath::new("/anything")));
    // a segment that was spelled to hide the dots still meets the pattern
    assert!(pattern_matches("/gists/**", "/x/%2e%2e/gists/1"));
    assert!(pattern_matches(
        "/repos/*/issues",
        "/repos/x%2f..%2fme/issues"
    ));
}

#[test]
fn methods_are_tokens_kept_upper_case_and_matched_without_case() {
    let methods = Methods::parse(["post", "POST", "Delete"]).unwrap();
    assert_eq!(methods.names(), ["DELETE", "POST"]);
    assert!(methods.matches_any(["Post"]));
    assert!(methods.matches_any(["GET", "delete"]));
    assert!(!methods.matches_any(["GET"]));
    assert!(!methods.matches_any([]));
    assert!(Methods::any().matches_any([]));
    for bad in ["", "GE T", "GET\n", "G,T", "a/b", &"X".repeat(33)] {
        assert!(Methods::parse([bad]).is_err(), "{bad:?}");
    }
    let many: Vec<String> = (0..=MAX_METHODS).map(|i| format!("M{i}")).collect();
    assert!(Methods::parse(many.iter().map(String::as_str)).is_err());
    assert_eq!(
        MethodError("x".into()).to_string(),
        "\"x\" is not an HTTP method"
    );
}

#[test]
fn the_target_is_cut_to_its_path_without_mistaking_a_scheme() {
    assert_eq!(target_path("/a/b?x=1#y"), "/a/b");
    assert_eq!(
        target_path("/redirect/http://evil/admin"),
        "/redirect/http://evil/admin"
    );
    assert_eq!(target_path("http://u:p@h.example/x?q"), "/x");
    assert_eq!(target_path("*"), "*");
    assert_eq!(canon(target_path("*")), "*");
}
