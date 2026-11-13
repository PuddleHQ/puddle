// SPDX-License-Identifier: GPL-3.0-or-later
use std::cell::Cell;

use super::*;

struct Fake {
    node: bool,
    calls: Cell<u32>,
}

impl TypeGenerator for Fake {
    fn available(&self) -> bool {
        self.node
    }

    fn generate(&self, spec: &str) -> Result<String> {
        self.calls.set(self.calls.get() + 1);
        Ok(format!("export type Spec = {};\n", spec.len()))
    }
}

fn fake(node: bool) -> Fake {
    Fake {
        node,
        calls: Cell::new(0),
    }
}

fn types_in(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("ui/src/lib/api").join(TYPES_FILE)
}

#[test]
fn write_then_check_passes_and_drift_fails() {
    let dir = tempfile::tempdir().unwrap();
    let types_path = types_in(&dir);
    let g = fake(true);
    let r = openapi(dir.path(), "{}\n", false, Types::Required, &g, &types_path).unwrap();
    assert_eq!(r.line(), "openapi.json written; schema.d.ts written");
    let types = std::fs::read_to_string(&types_path).unwrap();
    assert!(types.starts_with("// SPDX-License-Identifier: GPL-3.0-or-later\n"));
    let r = openapi(dir.path(), "{}\n", true, Types::Required, &g, &types_path).unwrap();
    assert_eq!(r.line(), "openapi.json is current; schema.d.ts is current");
    assert_eq!(g.calls.get(), 2);

    let err = openapi(dir.path(), "{ }\n", true, Types::Required, &g, &types_path).unwrap_err();
    assert!(
        matches!(err, XtaskError::Stale(ref p) if p.ends_with(SPEC_FILE)),
        "{err}"
    );
    assert!(err.to_string().contains("cargo xtask openapi"), "{err}");

    std::fs::write(&types_path, "edited by hand").unwrap();
    let err = openapi(dir.path(), "{}\n", true, Types::Required, &g, &types_path).unwrap_err();
    assert!(
        matches!(err, XtaskError::Stale(ref p) if p == &types_path),
        "{err}"
    );
}

/// The types have one committed copy, so two pull requests that add a route conflict in one
/// generated file and not in two.
#[test]
fn the_types_are_written_to_one_file_only() {
    let dir = tempfile::tempdir().unwrap();
    let types_path = types_in(&dir);
    openapi(
        dir.path(),
        "{}\n",
        false,
        Types::Required,
        &fake(true),
        &types_path,
    )
    .unwrap();
    assert!(types_path.is_file());
    assert!(!dir.path().join(TYPES_FILE).exists());
    let mut files: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    files.sort();
    assert_eq!(files, ["openapi.json", "ui"]);
}

#[test]
fn missing_node_skips_locally_and_fails_when_required() {
    let dir = tempfile::tempdir().unwrap();
    let types_path = types_in(&dir);
    std::fs::write(dir.path().join(SPEC_FILE), "{}\n").unwrap();
    let r = openapi(
        dir.path(),
        "{}\n",
        true,
        Types::IfNodePresent,
        &fake(false),
        &types_path,
    )
    .unwrap();
    assert!(r.types.starts_with("not checked"), "{}", r.line());
    let err = openapi(
        dir.path(),
        "{}\n",
        true,
        Types::Required,
        &fake(false),
        &types_path,
    )
    .unwrap_err();
    assert!(err.to_string().contains("Node is needed"), "{err}");
    // A stale spec is reported even without Node.
    let err = openapi(
        dir.path(),
        "[]\n",
        true,
        Types::IfNodePresent,
        &fake(false),
        &types_path,
    );
    assert!(matches!(err, Err(XtaskError::Stale(_))));
}

#[test]
fn generator_failures_surface() {
    let g = NodeGenerator {
        // flake-ok: a path that must not exist; on Windows it is rooted on the current drive and still does not exist
        dir: PathBuf::from("/nonexistent/puddle-openapi"),
        work: std::path::absolute("xtask-openapi-test").unwrap(),
    };
    if g.available() {
        assert!(g.generate("{}").is_err());
    }
}
