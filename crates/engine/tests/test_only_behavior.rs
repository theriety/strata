//! End-to-end behavior of the `testOnly` finding: a production item whose every
//! user is test code is advised once, and never when the claim could be false.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use strata_engine::{AnalyzeConfig, analyze, snapshot_from_root};

/// Writes a throwaway project holding `files` and returns its root.
fn write_project(files: &[(&str, &str)]) -> Result<PathBuf, String> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "strata-testonly-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    for (path, contents) in files {
        let target = root.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&target, contents).map_err(|error| error.to_string())?;
    }
    Ok(root)
}

/// The advice lines of one analysis: test-only and visibility.
struct Advice {
    /// Detail lines of `testOnly` findings.
    test_only: Vec<String>,
    /// Detail lines of visibility findings.
    visibility: Vec<String>,
}

/// Analyzes a throwaway project and returns its test-only and visibility advice.
fn advice(files: &[(&str, &str)]) -> Result<Advice, String> {
    let root = write_project(files)?;
    let config = AnalyzeConfig::default();
    let outcome = snapshot_from_root(&root, &config)
        .map_err(|error| error.to_string())
        .and_then(|snapshot| analyze(&snapshot, &config).map_err(|error| error.to_string()))
        .and_then(|result| serde_json::to_string_pretty(&result).map_err(|e| e.to_string()));
    let _ = fs::remove_dir_all(&root);
    let json = outcome?;
    let lines = |needle: &str| -> Vec<String> {
        json.lines()
            .filter(|line| line.contains(needle))
            .map(str::to_owned)
            .collect()
    };
    Ok(Advice {
        test_only: json
            .lines()
            .filter(|line| {
                line.contains("used only by tests") || line.contains("Only tests in this repo use")
            })
            .map(str::to_owned)
            .collect(),
        visibility: lines("exported at"),
    })
}

/// A Rust crate file list with the given `a.rs` body behind `mod a;`.
fn rust(lib: &str, a: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "Cargo.toml",
            "[package]\nname = \"to\"\nversion = \"0.1.0\"\nedition = \"2021\"\n".to_owned(),
        ),
        ("src/lib.rs", lib.to_owned()),
        ("src/a.rs", a.to_owned()),
    ]
}

/// Runs [`advice`] over owned file contents.
fn advice_owned(files: &[(&str, String)]) -> Result<Advice, String> {
    let borrowed: Vec<(&str, &str)> = files.iter().map(|(p, c)| (*p, c.as_str())).collect();
    advice(&borrowed)
}

/// Whether any line names `item` in backticks.
fn names(lines: &[String], item: &str) -> bool {
    lines.iter().any(|line| line.contains(&format!("`{item}`")))
}

#[test]
fn should_flag_a_crate_item_used_only_by_an_inline_test_module_without_a_visibility_finding()
-> Result<(), String> {
    let found = advice_owned(&rust(
        "mod a;\n",
        "pub(crate) fn helper() {}\n\n#[cfg(test)]\nmod tests {\n    use super::helper;\n\n    #[test]\n    fn it() {\n        helper();\n    }\n}\n",
    ))?;
    assert!(names(&found.test_only, "helper"), "{:?}", found.test_only);
    assert!(
        found
            .test_only
            .iter()
            .any(|line| line.contains("move it under test-only code or delete it")),
        "{:?}",
        found.test_only
    );
    assert!(
        !names(&found.visibility, "helper"),
        "{:?}",
        found.visibility
    );
    Ok(())
}

const PUBLIC_WORDING: &str = "It is public, so code outside this repo may use it too";

#[test]
fn should_flag_a_public_item_used_only_by_an_integration_test_with_the_public_wording()
-> Result<(), String> {
    let files = vec![
        (
            "Cargo.toml",
            "[package]\nname = \"to\"\nversion = \"0.1.0\"\nedition = \"2021\"\n".to_owned(),
        ),
        ("src/lib.rs", "pub fn helper() {}\n".to_owned()),
        (
            "tests/it.rs",
            "#[test]\nfn it() {\n    to::helper();\n}\n".to_owned(),
        ),
    ];
    let found = advice_owned(&files)?;
    assert!(names(&found.test_only, "helper"), "{:?}", found.test_only);
    assert!(
        found
            .test_only
            .iter()
            .any(|line| line.contains(PUBLIC_WORDING)),
        "{:?}",
        found.test_only
    );
    assert!(
        !names(&found.visibility, "helper"),
        "{:?}",
        found.visibility
    );
    Ok(())
}

#[test]
fn should_flag_an_item_named_only_inside_a_macro_of_a_test_module() -> Result<(), String> {
    let found = advice_owned(&rust(
        "mod a;\n",
        "pub(crate) fn helper() {}\n\n#[cfg(test)]\nmod tests {\n    use super::helper;\n\n    #[test]\n    fn plain() {\n        helper();\n    }\n\n    macro_rules! wrap {\n        ($e:expr) => {\n            #[test]\n            fn it() {\n                $e;\n            }\n        };\n    }\n\n    wrap! { helper() }\n}\n",
    ))?;
    assert!(names(&found.test_only, "helper"), "{:?}", found.test_only);
    Ok(())
}

#[test]
fn should_stay_silent_when_a_production_function_also_uses_the_item() -> Result<(), String> {
    let found = advice_owned(&rust(
        "mod a;\nfn go() {\n    a::run();\n}\n",
        "pub(crate) fn helper() {}\npub(crate) fn run() {\n    helper();\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn it() {\n        super::helper();\n    }\n}\n",
    ))?;
    assert!(!names(&found.test_only, "helper"), "{:?}", found.test_only);
    Ok(())
}

#[test]
fn should_stay_silent_when_a_feature_gated_item_uses_the_helper() -> Result<(), String> {
    // `drive` is feature-gated but the loader keeps it, so a real production
    // edge silences `helper`. This does not exercise the advisory path; the
    // item-macro test below is the proof for `settle` keying on `test_source`.
    let found = advice_owned(&rust(
        "mod a;\n",
        "fn helper() {}\n\n#[cfg(feature = \"extra\")]\nfn drive() {\n    helper();\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn it() {\n        super::helper();\n    }\n}\n",
    ))?;
    assert!(!names(&found.test_only, "helper"), "{:?}", found.test_only);
    Ok(())
}

#[test]
fn should_stay_silent_for_a_private_helper_used_by_a_feature_gated_item() -> Result<(), String> {
    let found = advice_owned(&rust(
        "mod a;\n",
        "fn helper() {}\n\n#[cfg(feature = \"extra\")]\nfn drive() {\n    helper();\n}\n\n#[cfg(test)]\nmod tests {\n    use super::helper;\n\n    #[test]\n    fn it() {\n        helper();\n    }\n}\n",
    ))?;
    assert!(!names(&found.test_only, "helper"), "{:?}", found.test_only);
    Ok(())
}

#[test]
fn should_stay_silent_for_a_private_helper_named_in_a_production_item_macro() -> Result<(), String>
{
    let found = advice_owned(&rust(
        "mod a;\n",
        "fn helper() -> u32 {\n    1\n}\n\nthread_local! {\n    static SLOT: u32 = helper();\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn it() {\n        super::helper();\n    }\n}\n",
    ))?;
    assert!(!names(&found.test_only, "helper"), "{:?}", found.test_only);
    Ok(())
}

#[test]
fn should_not_flag_a_cfg_test_helper_used_only_by_tests() -> Result<(), String> {
    let found = advice_owned(&rust(
        "mod a;\n",
        "#[cfg(test)]\nfn fixture() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn it() {\n        super::fixture();\n    }\n}\n",
    ))?;
    assert!(!names(&found.test_only, "fixture"), "{:?}", found.test_only);
    Ok(())
}

#[test]
fn should_not_flag_a_trait_impl_called_only_from_tests() -> Result<(), String> {
    let found = advice_owned(&rust(
        "mod a;\n",
        "pub(crate) struct S;\npub(crate) trait Tr {\n    fn f(&self);\n}\nimpl Tr for S {\n    fn f(&self) {}\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn it() {\n        S.f();\n    }\n}\n",
    ))?;
    assert!(
        !found
            .test_only
            .iter()
            .any(|line| line.contains("impl Tr for S")),
        "{:?}",
        found.test_only
    );
    Ok(())
}

#[test]
fn should_flag_a_typescript_export_used_only_by_a_spec_with_the_public_wording()
-> Result<(), String> {
    // exports count as public API; testOnly still speaks, in the public wording.
    let found = advice(&[
        (
            "src/util.ts",
            "export function helper(): number {\n  return 1;\n}\n",
        ),
        (
            "src/util.spec.ts",
            "import { helper } from \"./util\";\nexport const value: number = helper();\n",
        ),
    ])?;
    assert!(names(&found.test_only, "helper"), "{:?}", found.test_only);
    assert!(
        found
            .test_only
            .iter()
            .any(|line| line.contains(PUBLIC_WORDING)),
        "{:?}",
        found.test_only
    );
    assert_eq!(found.visibility, Vec::<String>::new());
    Ok(())
}

const PY_UTIL: &str =
    "def helper() -> int:\n    return 1\n\n\ndef _internal() -> int:\n    return 2\n";
const PY_TEST: &str = "from pkg.util import _internal, helper\n\n\ndef test_it() -> None:\n    assert helper() + _internal() == 3\n";

#[test]
fn should_flag_python_names_used_only_by_a_test_with_wording_by_publicity() -> Result<(), String> {
    let found = advice(&[
        ("pkg/__init__.py", ""),
        ("pkg/util.py", PY_UTIL),
        ("pkg/test_util.py", PY_TEST),
    ])?;
    assert!(
        names(&found.test_only, "_internal"),
        "{:?}",
        found.test_only
    );
    let line_for = |item: &str| {
        found
            .test_only
            .iter()
            .find(|line| line.contains(&format!("`{item}`")))
            .cloned()
            .unwrap_or_default()
    };
    assert!(!line_for("_internal").contains(PUBLIC_WORDING));
    assert!(names(&found.test_only, "helper"), "{:?}", found.test_only);
    assert!(line_for("helper").contains(PUBLIC_WORDING));
    assert!(!names(&found.test_only, "test_it"), "{:?}", found.test_only);
    Ok(())
}

/// `foo` lives in `a`, a production file reaches it through `prod`, and a test
/// reaches it directly: a bound production use means no testOnly finding.
fn silent_for(files: &[(&str, &str)], item: &str) -> Result<(), String> {
    let found = advice(files)?;
    assert!(!names(&found.test_only, item), "{:?}", found.test_only);
    Ok(())
}

const TS_A: &str = "export function foo(): number {\n  return 1;\n}\n";
const TS_SPEC: &str = "import { foo } from \"./a\";\nexport const value: number = foo();\n";

#[test]
fn should_stay_silent_for_a_ts_item_used_through_a_namespace_import() -> Result<(), String> {
    silent_for(
        &[
            ("src/a.ts", TS_A),
            (
                "src/prod.ts",
                "import * as ns from \"./a\";\nexport const used: number = ns.foo();\n",
            ),
            ("src/a.spec.ts", TS_SPEC),
        ],
        "foo",
    )
}

#[test]
fn should_stay_silent_for_a_ts_item_used_through_an_export_star_chain() -> Result<(), String> {
    silent_for(
        &[
            ("src/a.ts", TS_A),
            ("src/barrel.ts", "export * from \"./a\";\n"),
            (
                "src/prod.ts",
                "import { foo } from \"./barrel\";\nexport const used: number = foo();\n",
            ),
            ("src/a.spec.ts", TS_SPEC),
        ],
        "foo",
    )
}

#[test]
fn should_stay_silent_for_a_ts_item_used_through_require() -> Result<(), String> {
    silent_for(
        &[
            ("src/a.ts", TS_A),
            (
                "src/prod.ts",
                "const a = require(\"./a\");\nexport const used: number = a.foo();\n",
            ),
            ("src/a.spec.ts", TS_SPEC),
        ],
        "foo",
    )
}

const PY_A: &str = "def foo() -> int:\n    return 1\n";
const PY_A_TEST: &str =
    "from pkg.a import foo\n\n\ndef test_it() -> None:\n    assert foo() == 1\n";

#[test]
fn should_stay_silent_for_a_py_item_used_through_a_star_import() -> Result<(), String> {
    silent_for(
        &[
            ("pkg/__init__.py", ""),
            ("pkg/a.py", PY_A),
            (
                "pkg/prod.py",
                "from pkg.a import *\n\n\ndef run() -> int:\n    return foo()\n",
            ),
            ("pkg/test_a.py", PY_A_TEST),
        ],
        "foo",
    )
}

#[test]
fn should_stay_silent_for_a_py_item_used_through_a_module_attribute() -> Result<(), String> {
    silent_for(
        &[
            ("pkg/__init__.py", ""),
            ("pkg/a.py", PY_A),
            (
                "pkg/prod.py",
                "import pkg.a\n\n\ndef run() -> int:\n    return pkg.a.foo()\n",
            ),
            ("pkg/test_a.py", PY_A_TEST),
        ],
        "foo",
    )
}

#[test]
fn should_stay_silent_for_a_py_item_used_through_an_aliased_module() -> Result<(), String> {
    silent_for(
        &[
            ("pkg/__init__.py", ""),
            ("pkg/a.py", PY_A),
            (
                "pkg/prod.py",
                "import pkg.a as c\n\n\ndef run() -> int:\n    return c.foo()\n",
            ),
            ("pkg/test_a.py", PY_A_TEST),
        ],
        "foo",
    )
}

#[test]
fn should_stay_silent_for_a_py_item_used_through_a_from_imported_submodule() -> Result<(), String> {
    silent_for(
        &[
            ("pkg/__init__.py", ""),
            ("pkg/a.py", PY_A),
            (
                "pkg/prod.py",
                "from pkg import a\n\n\ndef run() -> int:\n    return a.foo()\n",
            ),
            ("pkg/test_a.py", PY_A_TEST),
        ],
        "foo",
    )
}

#[test]
fn should_stay_silent_for_a_py_item_used_through_a_relative_from_imported_submodule()
-> Result<(), String> {
    silent_for(
        &[
            ("pkg/__init__.py", ""),
            ("pkg/a.py", PY_A),
            (
                "pkg/prod.py",
                "from . import a\n\n\ndef run() -> int:\n    return a.foo()\n",
            ),
            ("pkg/test_a.py", PY_A_TEST),
        ],
        "foo",
    )
}

#[test]
fn should_flag_a_private_helper_without_the_gated_production_user() -> Result<(), String> {
    let found = advice_owned(&rust(
        "mod a;\n",
        "fn helper() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn it() {\n        super::helper();\n    }\n}\n",
    ))?;
    assert!(names(&found.test_only, "helper"), "{:?}", found.test_only);
    Ok(())
}
