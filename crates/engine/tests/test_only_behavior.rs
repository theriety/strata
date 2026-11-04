//! End-to-end behavior of the `testOnly` finding: a production item whose every
//! user is test code is advised once, and never when the claim could be false.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use unlace_engine::{AnalyzeConfig, analyze, snapshot_from_root};

/// Writes a throwaway project holding `files` and returns its root.
fn write_project(files: &[(&str, &str)]) -> Result<PathBuf, String> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "unlace-testonly-{}-{}",
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

/// The advice lines of one analysis: test-only, visibility and polarity.
struct Advice {
    /// Detail lines of `testOnly` findings.
    test_only: Vec<String>,
    /// Detail lines of visibility findings.
    visibility: Vec<String>,
    /// Detail lines of polarity findings: production depending on test code.
    polarity: Vec<String>,
}

/// Analyzes a throwaway project and returns its test-only and visibility advice.
fn advice(files: &[(&str, &str)]) -> Result<Advice, String> {
    advice_with(files, &AnalyzeConfig::default())
}

/// Like [`advice`], under an explicit `config`.
fn advice_with(files: &[(&str, &str)], config: &AnalyzeConfig) -> Result<Advice, String> {
    let root = write_project(files)?;
    let outcome = snapshot_from_root(&root, config)
        .map_err(|error| error.to_string())
        .and_then(|snapshot| analyze(&snapshot, config).map_err(|error| error.to_string()))
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
        polarity: lines("depends on test"),
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

/// A workspace of a shipped crate and an unpublished harness, each with one
/// public item that only its own integration test uses.
fn workspace(harness_manifest_extras: &str) -> Vec<(&'static str, String)> {
    let manifest = |name: &str, extras: &str| {
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n{extras}\n")
    };
    let test = |name: &str, item: &str| format!("#[test]\nfn it() {{\n    {name}::{item}();\n}}\n");
    vec![
        (
            "Cargo.toml",
            "[workspace]\nresolver = \"2\"\nmembers = [\"shipped\", \"harness\"]\n".to_owned(),
        ),
        ("shipped/Cargo.toml", manifest("shipped", "")),
        ("shipped/src/lib.rs", "pub fn normal_item() {}\n".to_owned()),
        ("shipped/tests/it.rs", test("shipped", "normal_item")),
        (
            "harness/Cargo.toml",
            manifest("harness", harness_manifest_extras),
        ),
        (
            "harness/src/lib.rs",
            "pub fn harness_item() {}\n".to_owned(),
        ),
        ("harness/tests/it.rs", test("harness", "harness_item")),
    ]
}

#[test]
fn should_skip_items_of_a_test_support_crate_but_still_flag_a_normal_crate() -> Result<(), String> {
    let found = advice_owned(&workspace("publish = false"))?;
    assert!(
        names(&found.test_only, "normal_item"),
        "{:?}",
        found.test_only
    );
    assert!(
        !names(&found.test_only, "harness_item"),
        "{:?}",
        found.test_only
    );
    Ok(())
}

#[test]
fn should_flag_a_published_crates_items_even_when_it_looks_like_a_harness() -> Result<(), String> {
    let found = advice_owned(&workspace(""))?;
    assert!(
        names(&found.test_only, "harness_item"),
        "{:?}",
        found.test_only
    );
    Ok(())
}

/// A TypeScript repo whose `spec/batch/utilities.ts` helper is used only by a spec.
const SPEC_HELPER: &str = "export function makeBatch(): number {\n  return 1;\n}\n";
const SPEC_USER: &str =
    "import { makeBatch } from \"./utilities\";\nexport const batch: number = makeBatch();\n";

/// The config with `patterns` as its test patterns.
fn with_patterns(patterns: &[&str]) -> AnalyzeConfig {
    let mut config = AnalyzeConfig::default();
    config.tests.patterns = patterns.iter().map(|&pattern| pattern.to_owned()).collect();
    config
}

#[test]
fn should_not_flag_a_typescript_helper_under_a_spec_root_used_only_by_a_spec() -> Result<(), String>
{
    let found = advice(&[
        ("spec/batch/utilities.ts", SPEC_HELPER),
        ("spec/batch/x.spec.ts", SPEC_USER),
    ])?;
    assert_eq!(found.test_only, Vec::<String>::new());
    Ok(())
}

#[test]
fn should_still_flag_a_src_item_used_only_by_a_spec_under_a_spec_root() -> Result<(), String> {
    let found = advice(&[
        ("src/batch.ts", SPEC_HELPER),
        (
            "spec/batch/x.spec.ts",
            "import { makeBatch } from \"../../src/batch\";\nexport const batch: number = makeBatch();\n",
        ),
    ])?;
    assert!(
        names(&found.test_only, "makeBatch"),
        "{:?}",
        found.test_only
    );
    Ok(())
}

#[test]
fn should_measure_the_spec_root_from_the_package_manifest_in_a_monorepo() -> Result<(), String> {
    let found = advice(&[
        ("packages/foo/package.json", "{}"),
        ("packages/foo/spec/h.ts", SPEC_HELPER),
        (
            "packages/foo/spec/x.spec.ts",
            &SPEC_USER.replace("./utilities", "./h"),
        ),
    ])?;
    assert_eq!(found.test_only, Vec::<String>::new());
    Ok(())
}

#[test]
fn should_report_a_helper_a_negated_pattern_takes_back_from_test_code() -> Result<(), String> {
    let files = [
        ("spec/fixtures/utilities.ts", SPEC_HELPER),
        ("spec/fixtures/x.spec.ts", SPEC_USER),
    ];
    let kept = advice_with(&files, &with_patterns(&[]))?;
    assert_eq!(kept.test_only, Vec::<String>::new());

    let found = advice_with(&files, &with_patterns(&["!spec/fixtures/utilities.ts"]))?;
    assert!(
        names(&found.test_only, "makeBatch"),
        "{:?}",
        found.test_only
    );
    Ok(())
}

#[test]
fn should_let_a_positive_pattern_mark_a_production_path_as_test_code() -> Result<(), String> {
    let files = [
        ("src/batch.ts", SPEC_HELPER),
        (
            "checks/batch.ts",
            &SPEC_USER.replace("./utilities", "../src/batch"),
        ),
    ];
    let plain = advice_with(&files, &with_patterns(&[]))?;
    assert!(!names(&plain.test_only, "makeBatch"));
    let found = advice_with(&files, &with_patterns(&["checks/**"]))?;
    assert!(
        names(&found.test_only, "makeBatch"),
        "{:?}",
        found.test_only
    );
    Ok(())
}

/// The config with `patterns` and `builtins` set.
fn with_tests(patterns: &[&str], builtins: bool) -> AnalyzeConfig {
    let mut config = with_patterns(patterns);
    config.tests.builtins = builtins;
    config
}

#[test]
fn should_let_only_patterns_decide_the_test_only_finding_when_builtins_are_off()
-> Result<(), String> {
    let files = [
        ("spec/batch/utilities.ts", SPEC_HELPER),
        ("spec/batch/x.spec.ts", SPEC_USER),
    ];
    // nothing is test code: the spec is a production user of the helper.
    let none = advice_with(&files, &with_tests(&[], false))?;
    assert_eq!(none.test_only, Vec::<String>::new());

    // the pattern marks the spec but not its helper: the helper is reported.
    let found = advice_with(&files, &with_tests(&["*.spec.ts"], false))?;
    assert!(
        names(&found.test_only, "makeBatch"),
        "{:?}",
        found.test_only
    );

    // a pattern that also marks the helper takes it out again.
    let both = advice_with(&files, &with_tests(&["*.spec.ts", "spec/**"], false))?;
    assert_eq!(both.test_only, Vec::<String>::new());

    // with builtins on, the spec root already covers the helper.
    let builtin = advice_with(&files, &with_tests(&[], true))?;
    assert_eq!(builtin.test_only, Vec::<String>::new());
    Ok(())
}

#[test]
fn should_report_a_helper_under_a_nested_spec_folder_of_src_as_production() -> Result<(), String> {
    let found = advice(&[
        ("src/spec/h.ts", SPEC_HELPER),
        (
            "src/spec/x.spec.ts",
            &SPEC_USER.replace("./utilities", "./h"),
        ),
    ])?;
    assert!(
        names(&found.test_only, "makeBatch"),
        "{:?}",
        found.test_only
    );
    Ok(())
}

/// A production module that imports the `makeBatch` helper from `path`.
fn production_user_of(path: &str) -> String {
    format!("import {{ makeBatch }} from \"{path}\";\nexport const run: number = makeBatch();\n")
}

#[test]
fn should_not_flag_a_helper_when_its_only_spec_user_is_negated() -> Result<(), String> {
    let files = [
        ("src/batch.ts", SPEC_HELPER),
        (
            "spec/batch/x.spec.ts",
            "import { makeBatch } from \"../../src/batch\";\nexport const batch: number = makeBatch();\n",
        ),
    ];
    let kept = advice_with(&files, &with_patterns(&[]))?;
    assert!(names(&kept.test_only, "makeBatch"), "{:?}", kept.test_only);

    let found = advice_with(&files, &with_patterns(&["!spec/batch/x.spec.ts"]))?;
    assert_eq!(
        found.test_only,
        Vec::<String>::new(),
        "a negated spec is a production user"
    );
    Ok(())
}

#[test]
fn should_let_a_negated_pattern_beat_a_positive_one_for_the_user() -> Result<(), String> {
    let files = [
        ("src/batch.ts", SPEC_HELPER),
        (
            "checks/batch.ts",
            &SPEC_USER.replace("./utilities", "../src/batch"),
        ),
    ];
    let positive = advice_with(&files, &with_patterns(&["checks/**"]))?;
    assert!(names(&positive.test_only, "makeBatch"));

    let negated = advice_with(&files, &with_patterns(&["checks/**", "!checks/batch.ts"]))?;
    assert_eq!(negated.test_only, Vec::<String>::new());
    Ok(())
}

#[test]
fn should_report_the_items_of_a_test_support_crate_a_negated_pattern_takes_back()
-> Result<(), String> {
    let owned = workspace("publish = false");
    let files: Vec<(&str, &str)> = owned.iter().map(|(p, c)| (*p, c.as_str())).collect();
    let skipped = advice_with(&files, &with_patterns(&[]))?;
    assert!(!names(&skipped.test_only, "harness_item"));

    let found = advice_with(&files, &with_patterns(&["!harness/src/**"]))?;
    assert!(
        names(&found.test_only, "harness_item"),
        "{:?}",
        found.test_only
    );
    Ok(())
}

#[test]
fn should_stop_skipping_test_support_crates_when_builtins_are_off() -> Result<(), String> {
    let owned = workspace("publish = false");
    let files: Vec<(&str, &str)> = owned.iter().map(|(p, c)| (*p, c.as_str())).collect();
    let found = advice_with(&files, &with_tests(&["**/tests/**"], false))?;
    assert!(
        names(&found.test_only, "harness_item"),
        "{:?}",
        found.test_only
    );
    Ok(())
}

#[test]
fn should_stop_calling_a_production_import_of_a_negated_helper_a_polarity_breach()
-> Result<(), String> {
    let user = production_user_of("../spec/fixtures/utilities");
    let files = [
        ("spec/fixtures/utilities.ts", SPEC_HELPER),
        ("src/app.ts", user.as_str()),
    ];
    let kept = advice_with(&files, &with_patterns(&[]))?;
    assert!(!kept.polarity.is_empty(), "the spec root is test code");

    let found = advice_with(&files, &with_patterns(&["!spec/fixtures/**"]))?;
    assert_eq!(found.polarity, Vec::<String>::new());
    Ok(())
}

#[test]
fn should_call_a_production_import_of_a_pattern_marked_file_a_polarity_breach() -> Result<(), String>
{
    let user = production_user_of("../checks/batch");
    let files = [
        ("checks/batch.ts", SPEC_HELPER),
        ("src/app.ts", user.as_str()),
    ];
    let plain = advice_with(&files, &with_patterns(&[]))?;
    assert_eq!(plain.polarity, Vec::<String>::new());

    let found = advice_with(&files, &with_patterns(&["checks/**"]))?;
    assert!(!found.polarity.is_empty(), "{:?}", found.polarity);
    Ok(())
}

#[test]
fn should_drop_adapter_polarity_breaches_when_builtins_are_off() -> Result<(), String> {
    let user = production_user_of("../spec/fixtures/utilities");
    let files = [
        ("spec/fixtures/utilities.ts", SPEC_HELPER),
        ("src/app.ts", user.as_str()),
    ];
    let found = advice_with(&files, &with_tests(&[], false))?;
    assert_eq!(found.polarity, Vec::<String>::new());
    Ok(())
}

/// The layout where a pattern-marked folder meets a built-in test root.
fn pattern_meets_test_root(helper_imports_util: bool) -> Vec<(&'static str, String)> {
    let util = if helper_imports_util {
        SPEC_HELPER.to_owned()
    } else {
        "import { checkIt } from \"../checks/helper\";\nexport function makeBatch(): number {\n  return checkIt();\n}\n"
            .to_owned()
    };
    let helper = if helper_imports_util {
        "import { makeBatch } from \"../spec/util\";\nexport function checkIt(): number {\n  return makeBatch();\n}\n"
            .to_owned()
    } else {
        "export function checkIt(): number {\n  return 1;\n}\n".to_owned()
    };
    vec![
        ("package.json", "{}".to_owned()),
        ("spec/util.ts", util),
        ("checks/helper.ts", helper),
        (
            "checks/run.spec.ts",
            "import { checkIt } from \"./helper\";\nexport const run: number = checkIt();\n"
                .to_owned(),
        ),
    ]
}

/// Runs [`advice_with`] over owned file contents.
fn advice_owned_with(files: &[(&str, String)], config: &AnalyzeConfig) -> Result<Advice, String> {
    let borrowed: Vec<(&str, &str)> = files.iter().map(|(p, c)| (*p, c.as_str())).collect();
    advice_with(&borrowed, config)
}

#[test]
fn should_not_flag_a_pattern_marked_helper_that_imports_a_built_in_test_helper()
-> Result<(), String> {
    let found = advice_owned_with(
        &pattern_meets_test_root(true),
        &with_patterns(&["checks/**"]),
    )?;
    assert_eq!(found.polarity, Vec::<String>::new());
    Ok(())
}

#[test]
fn should_not_flag_a_built_in_test_helper_that_imports_a_pattern_marked_helper()
-> Result<(), String> {
    let found = advice_owned_with(
        &pattern_meets_test_root(false),
        &with_patterns(&["checks/**"]),
    )?;
    assert_eq!(found.polarity, Vec::<String>::new());
    Ok(())
}

#[test]
fn should_still_flag_a_test_support_helper_that_imports_a_spec_file() -> Result<(), String> {
    let found = advice(&[
        ("package.json", "{}"),
        (
            "spec/util.ts",
            "import { batch } from \"./x.spec\";\nexport function makeBatch(): number {\n  return batch;\n}\n",
        ),
        ("spec/x.spec.ts", "export const batch: number = 1;\n"),
    ])?;
    assert!(
        found
            .polarity
            .iter()
            .any(|line| line.contains("test support") && line.contains("test case")),
        "{:?}",
        found.polarity
    );
    Ok(())
}

#[test]
fn should_flag_a_helper_used_only_by_a_spec_with_an_mts_or_cts_extension() -> Result<(), String> {
    for (spec, import) in [("src/flow.spec.mts", "./h"), ("src/flow.test.cts", "./h")] {
        let user = SPEC_USER.replace("./utilities", import);
        let found = advice(&[("src/h.ts", SPEC_HELPER), (spec, user.as_str())])?;
        assert!(
            names(&found.test_only, "makeBatch"),
            "{spec}: {:?}",
            found.test_only
        );
    }
    Ok(())
}
