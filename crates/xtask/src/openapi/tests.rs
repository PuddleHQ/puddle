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

#[test]
fn write_then_check_passes_and_drift_fails() {
    let dir = tempfile::tempdir().unwrap();
    let g = fake(true);
    let r = openapi(dir.path(), "{}\n", false, Types::Required, &g, &[]).unwrap();
    assert_eq!(r.line(), "openapi.json written; schema.d.ts written");
    let types = std::fs::read_to_string(dir.path().join(TYPES_FILE)).unwrap();
    assert!(types.starts_with("// SPDX-License-Identifier: GPL-3.0-or-later\n"));
    let r = openapi(dir.path(), "{}\n", true, Types::Required, &g, &[]).unwrap();
    assert_eq!(r.line(), "openapi.json is current; schema.d.ts is current");
    assert_eq!(g.calls.get(), 2);

    let err = openapi(dir.path(), "{ }\n", true, Types::Required, &g, &[]).unwrap_err();
    assert!(
        matches!(err, XtaskError::Stale(ref p) if p.ends_with(SPEC_FILE)),
        "{err}"
    );
    assert!(err.to_string().contains("cargo xtask openapi"), "{err}");

    std::fs::write(dir.path().join(TYPES_FILE), "edited by hand").unwrap();
    let err = openapi(dir.path(), "{}\n", true, Types::Required, &g, &[]).unwrap_err();
    assert!(
        matches!(err, XtaskError::Stale(ref p) if p.ends_with(TYPES_FILE)),
        "{err}"
    );
}

#[test]
fn extra_copies_are_written_and_checked_too() {
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("ui/src/lib/api/schema.d.ts");
    let copies = [copy.clone()];
    let g = fake(true);
    openapi(dir.path(), "{}\n", false, Types::Required, &g, &copies).unwrap();
    // The copy's folder is created, and the copy equals the main file.
    assert_eq!(
        std::fs::read_to_string(&copy).unwrap(),
        std::fs::read_to_string(dir.path().join(TYPES_FILE)).unwrap()
    );
    openapi(dir.path(), "{}\n", true, Types::Required, &g, &copies).unwrap();
    std::fs::write(&copy, "edited by hand").unwrap();
    let err = openapi(dir.path(), "{}\n", true, Types::Required, &g, &copies).unwrap_err();
    assert!(
        matches!(err, XtaskError::Stale(ref p) if p == &copy),
        "{err}"
    );
}

#[test]
fn missing_node_skips_locally_and_fails_when_required() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(SPEC_FILE), "{}\n").unwrap();
    let r = openapi(
        dir.path(),
        "{}\n",
        true,
        Types::IfNodePresent,
        &fake(false),
        &[],
    )
    .unwrap();
    assert!(r.types.starts_with("not checked"), "{}", r.line());
    let err = openapi(dir.path(), "{}\n", true, Types::Required, &fake(false), &[]).unwrap_err();
    assert!(err.to_string().contains("Node is needed"), "{err}");
    // A stale spec is reported even without Node.
    let err = openapi(
        dir.path(),
        "[]\n",
        true,
        Types::IfNodePresent,
        &fake(false),
        &[],
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
