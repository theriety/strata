//! Python file-import facts (decision E3): the adapter states which analysed
//! file each load-time import statement loads, and says when it listed them all.

use std::fs;
use std::path::Path;

use smol_str::SmolStr;
use unlace_adapter_python::PythonAdapter;
use unlace_ir::{Adapter, FileImport, ImportKind, IrFragment, ScopeLevel, SourceFile};

/// Parses and binds `(path, contents)` pairs into one fragment.
fn fragment(files: &[(&str, &str)]) -> IrFragment {
    let adapter = PythonAdapter::default();
    let sources: Vec<SourceFile> = files
        .iter()
        .map(|(path, contents)| SourceFile {
            path: SmolStr::new(*path),
            contents: (*contents).to_owned(),
        })
        .collect();
    let trees = adapter.parse(&sources).unwrap_or_default();
    adapter.bind(trees).unwrap_or_default()
}

/// The facts of `importer` as `(imported, specifier, line)` triples.
fn facts_of(fragment: &IrFragment, importer: &str) -> Vec<(String, String, u32)> {
    fragment
        .imports
        .iter()
        .filter(|import| import.importer == importer)
        .map(|import| {
            (
                import.imported.to_string(),
                import.specifier.to_string(),
                import.line,
            )
        })
        .collect()
}

/// One expected fact.
fn expect(imported: &str, specifier: &str, line: u32) -> (String, String, u32) {
    (imported.to_owned(), specifier.to_owned(), line)
}

const BARREL: (&str, &str) = ("pkg/__init__.py", "SETTING = 1\n");

#[test]
fn should_emit_a_value_import_for_every_module_form_with_its_line_and_specifier() {
    let app = "import pkg.b\nfrom pkg.b import thing\nfrom . import c\nfrom .d import helper\nfrom .sub import deep\nfrom pkg import b\nfrom pkg import SETTING\nimport pkg.b as alias\n";
    let fragment = fragment(&[
        BARREL,
        ("pkg/app.py", app),
        ("pkg/b.py", "thing = 1\n"),
        ("pkg/c.py", "value = 1\n"),
        ("pkg/d.py", "def helper():\n    pass\n"),
        ("pkg/sub/__init__.py", "deep = 1\n"),
    ]);

    assert_eq!(
        facts_of(&fragment, "pkg/app.py"),
        vec![
            expect("pkg/__init__.py", "pkg", 7),
            expect("pkg/b.py", "pkg", 6),
            expect("pkg/b.py", "pkg.b", 1),
            expect("pkg/b.py", "pkg.b", 2),
            expect("pkg/b.py", "pkg.b", 8),
            expect("pkg/c.py", ".", 3),
            expect("pkg/d.py", ".d", 4),
            expect("pkg/sub/__init__.py", ".sub", 5),
        ]
    );
    assert!(
        fragment
            .imports
            .iter()
            .all(|import| import.kind == ImportKind::Value)
    );
    assert!(fragment.states_imports);
}

#[test]
fn should_resolve_a_star_import_and_a_parent_relative_import() {
    let fragment = fragment(&[
        BARREL,
        (
            "pkg/sub/app.py",
            "from ..util import *\nfrom .. import helper\n",
        ),
        ("pkg/util.py", "X = 1\n"),
        ("pkg/helper.py", "Y = 1\n"),
    ]);

    assert_eq!(
        facts_of(&fragment, "pkg/sub/app.py"),
        vec![
            expect("pkg/helper.py", "..", 2),
            expect("pkg/util.py", "..util", 1)
        ]
    );
    assert!(fragment.states_imports);
}

#[test]
fn should_leave_out_a_type_checking_only_import() {
    let source = "from typing import TYPE_CHECKING\nimport typing\nfrom .real import R\nif TYPE_CHECKING:\n    from .only_types import T\n    import pkg.also_types\nelse:\n    from .fallback import F\nif typing.TYPE_CHECKING:\n    from .qualified import Q\nif not TYPE_CHECKING:\n    from .runtime import RT\nelse:\n    from .checker import C\n";
    let fragment = fragment(&[
        BARREL,
        ("pkg/app.py", source),
        ("pkg/real.py", "R = 1\n"),
        ("pkg/only_types.py", "T = 1\n"),
        ("pkg/also_types.py", "A = 1\n"),
        ("pkg/fallback.py", "F = 1\n"),
        ("pkg/qualified.py", "Q = 1\n"),
        ("pkg/runtime.py", "RT = 1\n"),
        ("pkg/checker.py", "C = 1\n"),
    ]);

    assert_eq!(
        facts_of(&fragment, "pkg/app.py"),
        vec![
            expect("pkg/fallback.py", ".fallback", 8),
            expect("pkg/real.py", ".real", 3),
            expect("pkg/runtime.py", ".runtime", 12),
        ]
    );
    assert!(fragment.states_imports);
}

#[test]
fn should_leave_out_a_lazy_import_inside_a_function_body() {
    let source = "from .top import T\n\n\ndef load():\n    from .lazy import L\n    import pkg.lazier\n    return L\n\n\nclass Holder:\n    def method(self):\n        from .lazy import L\n";
    let fragment = fragment(&[
        BARREL,
        ("pkg/app.py", source),
        ("pkg/top.py", "T = 1\n"),
        ("pkg/lazy.py", "L = 1\n"),
        ("pkg/lazier.py", "M = 1\n"),
    ]);

    assert_eq!(
        facts_of(&fragment, "pkg/app.py"),
        vec![expect("pkg/top.py", ".top", 1)]
    );
    assert!(fragment.states_imports);
}

#[test]
fn should_keep_imports_in_try_with_and_loop_blocks_that_run_at_load_time() {
    let source = "try:\n    from ._speedups import fast\nexcept ImportError:\n    from ._pure import fast\nwith guard:\n    from ._with import w\n";
    let fragment = fragment(&[
        BARREL,
        ("pkg/app.py", source),
        ("pkg/_speedups.py", "fast = 1\n"),
        ("pkg/_pure.py", "fast = 1\n"),
        ("pkg/_with.py", "w = 1\n"),
    ]);

    assert_eq!(
        facts_of(&fragment, "pkg/app.py"),
        vec![
            expect("pkg/_pure.py", "._pure", 4),
            expect("pkg/_speedups.py", "._speedups", 2),
            expect("pkg/_with.py", "._with", 6),
        ]
    );
}

#[test]
fn should_emit_nothing_for_an_external_package_and_still_state_imports() {
    let fragment = fragment(&[
        BARREL,
        (
            "pkg/app.py",
            "import os\nimport numpy.linalg\nfrom requests import get\nfrom .b import B\n",
        ),
        ("pkg/b.py", "B = 1\n"),
    ]);

    assert_eq!(
        facts_of(&fragment, "pkg/app.py"),
        vec![expect("pkg/b.py", ".b", 4)]
    );
    assert!(fragment.states_imports);
}

#[test]
fn should_resolve_a_submodule_of_a_namespace_package_but_not_the_package_itself() {
    let fragment = fragment(&[
        ("app.py", "import ns\nfrom ns import mod\nimport ns.mod\n"),
        ("ns/mod.py", "M = 1\n"),
    ]);

    assert_eq!(
        facts_of(&fragment, "app.py"),
        vec![
            expect("ns/mod.py", "ns", 2),
            expect("ns/mod.py", "ns.mod", 3)
        ]
    );
    assert!(fragment.states_imports);
}

#[test]
fn should_not_state_imports_when_an_import_inside_the_analysed_set_resolves_to_no_file() {
    for source in [
        "from .ghost import x\n",
        "import pkg.ghost\n",
        "from pkg.ghost import x\n",
        "from ... import x\n",
    ] {
        let fragment = fragment(&[BARREL, ("pkg/app.py", source), ("pkg/b.py", "B = 1\n")]);

        assert!(!fragment.states_imports, "{source}");
    }
}

#[test]
fn should_still_emit_the_resolved_facts_when_another_import_is_unresolved() {
    let fragment = fragment(&[
        BARREL,
        ("pkg/app.py", "from .b import B\nfrom .ghost import G\n"),
        ("pkg/b.py", "B = 1\n"),
    ]);

    assert_eq!(
        facts_of(&fragment, "pkg/app.py"),
        vec![expect("pkg/b.py", ".b", 1)]
    );
    assert!(!fragment.states_imports);
}

#[test]
fn should_emit_both_directions_of_an_import_cycle_in_a_deterministic_order() {
    let files = [
        BARREL,
        ("pkg/one.py", "from .two import b\na = 1\n"),
        ("pkg/two.py", "from .one import a\nb = 1\n"),
    ];

    let first = fragment(&files).imports;
    let second = fragment(&files).imports;

    assert_eq!(first, second);
    assert_eq!(
        first,
        vec![
            FileImport {
                importer: SmolStr::new("pkg/one.py"),
                imported: SmolStr::new("pkg/two.py"),
                kind: ImportKind::Value,
                specifier: SmolStr::new(".two"),
                line: 1,
            },
            FileImport {
                importer: SmolStr::new("pkg/two.py"),
                imported: SmolStr::new("pkg/one.py"),
                kind: ImportKind::Value,
                specifier: SmolStr::new(".one"),
                line: 1,
            },
        ]
    );
}

#[test]
fn should_spell_both_paths_exactly_like_the_file_containers() {
    let fragment = fragment(&[
        BARREL,
        ("pkg/app.py", "from .b import B\nfrom . import sub\n"),
        ("pkg/b.py", "B = 1\n"),
        ("pkg/sub/__init__.py", "S = 1\n"),
    ]);

    let files: Vec<&str> = fragment
        .containers
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .map(|container| container.name.as_str())
        .collect();
    assert!(!fragment.imports.is_empty());
    for import in &fragment.imports {
        assert!(files.contains(&import.importer.as_str()), "{import:?}");
        assert!(files.contains(&import.imported.as_str()), "{import:?}");
    }
}

/// Collects every `.py` file below `dir` as `(path relative to base, contents)`.
fn python_files(base: &Path, dir: &Path, found: &mut Vec<(String, String)>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            python_files(base, &path, found);
        } else if path.extension().is_some_and(|extension| extension == "py")
            && let (Ok(relative), Ok(contents)) =
                (path.strip_prefix(base), fs::read_to_string(&path))
        {
            found.push((relative.to_string_lossy().replace('\\', "/"), contents));
        }
    }
}

/// A4: every non-external import of every Python eval fixture resolves.
#[test]
fn should_state_imports_for_every_python_eval_fixture() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../eval/fixtures");
    let mut checked = 0;
    let mut roots: Vec<_> = fs::read_dir(&fixtures)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    roots.sort();
    for root in roots.iter().filter(|root| root.is_dir()) {
        let mut found = Vec::new();
        python_files(root, root, &mut found);
        if found.is_empty() {
            continue;
        }
        let pairs: Vec<(&str, &str)> = found
            .iter()
            .map(|(path, contents)| (path.as_str(), contents.as_str()))
            .collect();
        let fragment = fragment(&pairs);

        assert!(fragment.states_imports, "{}", root.display());
        checked += 1;
    }
    assert!(checked > 0, "no Python eval fixture found");
}
