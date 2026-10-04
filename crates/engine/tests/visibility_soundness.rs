//! End-to-end soundness regressions for visibility findings (ADR-25).
//!
//! A visibility finding claims a narrower spelling compiles cleanly, so each
//! case below pins one way the claim used to be false (or must stay true): a
//! type exposed through a wider signature, a rung that is the declared one, an
//! item used from another crate target, and languages with no ladder.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use strata_engine::{AnalyzeConfig, analyze, snapshot_from_root};

/// Writes a throwaway project holding `files` and returns its root.
fn write_project(files: &[(&str, &str)], manifest: bool) -> Result<PathBuf, String> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "strata-soundness-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let cargo = "[package]\nname = \"sound\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
    let extra: &[(&str, &str)] = if manifest {
        &[("Cargo.toml", cargo)]
    } else {
        &[]
    };
    for (path, contents) in files.iter().chain(extra) {
        let target = root.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&target, contents).map_err(|error| error.to_string())?;
    }
    Ok(root)
}

/// Returns every visibility advice line the analysis states for the project.
fn advice_of(root: &Path) -> Result<Vec<String>, String> {
    let config = AnalyzeConfig::default();
    let snapshot = snapshot_from_root(root, &config).map_err(|error| error.to_string())?;
    let result = analyze(&snapshot, &config).map_err(|error| error.to_string())?;
    let json = serde_json::to_string_pretty(&result).map_err(|error| error.to_string())?;
    Ok(json
        .lines()
        .filter(|line| line.contains("exported at"))
        .map(str::to_owned)
        .collect())
}

/// Analyzes a throwaway crate and cleans it up.
fn rust_advice(files: &[(&str, &str)]) -> Result<Vec<String>, String> {
    let root = write_project(files, true)?;
    let advice = advice_of(&root);
    let _ = fs::remove_dir_all(&root);
    advice
}

/// Whether any advice line names `item` in backticks.
fn mentions(advice: &[String], item: &str) -> bool {
    advice
        .iter()
        .any(|line| line.contains(&format!("`{item}`")))
}

#[test]
fn should_not_flag_a_bound_trait_exposed_by_a_wider_generic_signature() -> Result<(), String> {
    // group E: `Bound` appears only in `f`'s generic bound; narrowing it to
    // private would trip private_bounds because `f` is crate-wide.
    let advice = rust_advice(&[
        ("src/lib.rs", "mod a;\nfn go() {\n    a::f::<u8>();\n}\n"),
        (
            "src/a.rs",
            "pub(crate) trait Bound {}\nimpl Bound for u8 {}\npub(crate) fn f<T: Bound>() {}\n",
        ),
    ])?;
    assert!(!mentions(&advice, "Bound"), "{advice:?}");
    Ok(())
}

#[test]
fn should_not_flag_a_type_returned_by_a_wider_signature() -> Result<(), String> {
    let advice = rust_advice(&[
        ("src/lib.rs", "mod a;\nfn go() {\n    let _ = a::g();\n}\n"),
        (
            "src/a.rs",
            "pub(crate) struct Ty;\npub(crate) fn g() -> Ty {\n    Ty\n}\n",
        ),
    ])?;
    assert!(!mentions(&advice, "Ty"), "{advice:?}");
    Ok(())
}

#[test]
fn should_still_flag_a_type_used_only_in_a_function_body() -> Result<(), String> {
    // control: `Helper` never reaches a signature, so private still compiles.
    let advice = rust_advice(&[
        ("src/lib.rs", "mod a;\nfn go() {\n    a::h();\n}\n"),
        (
            "src/a.rs",
            "pub(crate) struct Helper {}\npub(crate) fn h() {\n    let _x: Helper = Helper {};\n}\n",
        ),
    ])?;
    let line = advice
        .iter()
        .find(|line| line.contains("`Helper`"))
        .ok_or_else(|| format!("Helper must stay flagged: {advice:?}"))?;
    assert!(
        line.contains("narrowest spelling: private (no `pub`)"),
        "{line}"
    );
    Ok(())
}

#[test]
fn should_not_flag_a_pub_super_item_whose_consumer_is_the_cfg_test_parent() -> Result<(), String> {
    // group F: `pub(super)` from `render/x.rs` is module `render`, which holds
    // `render.rs`; private would break the `cfg(test)` caller there.
    let advice = rust_advice(&[
        ("src/lib.rs", "mod render;\n"),
        (
            "src/render.rs",
            "mod x;\n#[cfg(test)]\nfn check() -> usize {\n    x::helper()\n}\n",
        ),
        (
            "src/render/x.rs",
            "pub(super) fn helper() -> usize {\n    1\n}\n",
        ),
    ])?;
    assert!(!mentions(&advice, "helper"), "{advice:?}");
    Ok(())
}

#[test]
fn should_not_flag_a_restricted_item_already_at_its_needed_scope() -> Result<(), String> {
    // group G: `pub(in crate::a)` used only inside `crate::a` is a no-op.
    let advice = rust_advice(&[
        ("src/lib.rs", "mod a;\n"),
        (
            "src/a.rs",
            "mod inner;\npub(crate) fn run() -> usize {\n    inner::f()\n}\n",
        ),
        (
            "src/a/inner.rs",
            "pub(in crate::a) fn f() -> usize {\n    1\n}\n",
        ),
    ])?;
    assert!(!mentions(&advice, "f"), "{advice:?}");
    Ok(())
}

#[test]
fn should_not_flag_a_pub_super_item_used_by_its_sibling() -> Result<(), String> {
    // group G: the need maps to the declared rung, so there is nothing to
    // narrow and certainly no widening to advise.
    let advice = rust_advice(&[
        ("src/lib.rs", "mod x;\n"),
        ("src/x.rs", "mod y;\nmod z;\n"),
        (
            "src/x/y.rs",
            "pub(super) fn shared() -> usize {\n    1\n}\n",
        ),
        (
            "src/x/z.rs",
            "pub(super) fn user() -> usize {\n    super::y::shared()\n}\n",
        ),
    ])?;
    assert!(!mentions(&advice, "shared"), "{advice:?}");
    Ok(())
}

#[test]
fn should_not_flag_a_pub_fn_whose_only_outside_user_is_an_integration_test() -> Result<(), String> {
    // F73: `tests/eval.rs` is a separate crate target, in no rung of the
    // library's module ladder, so no spelling is known to reach it.
    let advice = rust_advice(&[
        ("src/lib.rs", "pub mod util;\n"),
        ("src/util.rs", "pub fn fixture_root() -> u8 {\n    1\n}\n"),
        (
            "tests/eval.rs",
            "#[test]\nfn finds_root() {\n    assert_eq!(sound::util::fixture_root(), 1);\n}\n",
        ),
    ])?;
    assert!(!mentions(&advice, "fixture_root"), "{advice:?}");
    Ok(())
}

#[test]
fn should_flag_a_pub_crate_item_used_only_in_its_own_module_with_a_spelling() -> Result<(), String>
{
    let advice = rust_advice(&[
        ("src/lib.rs", "mod a;\nfn go() {\n    a::run();\n}\n"),
        (
            "src/a.rs",
            "pub(crate) fn run() {\n    helper();\n}\npub(crate) fn helper() {}\n",
        ),
    ])?;
    let line = advice
        .iter()
        .find(|line| line.contains("`helper`"))
        .ok_or_else(|| format!("helper must be flagged: {advice:?}"))?;
    assert!(
        line.contains("narrowest spelling: private (no `pub`)"),
        "{line}"
    );
    assert!(!mentions(&advice, "run"), "{advice:?}");
    Ok(())
}

#[test]
fn should_not_flag_an_item_in_an_inline_module_used_by_its_parent_file() -> Result<(), String> {
    // an inline `mod imp` shares `lib.rs` with its parent: private would hide
    // `f` from the parent, so no narrower spelling exists.
    let advice = rust_advice(&[
        (
            "src/lib.rs",
            "mod a;\nmod imp {\n    pub(crate) fn f() {}\n}\nfn go() {\n    imp::f();\n    a::g();\n}\n",
        ),
        ("src/a.rs", "pub(crate) fn g() {}\n"),
    ])?;
    assert!(!mentions(&advice, "f"), "{advice:?}");
    Ok(())
}

#[test]
fn should_not_flag_items_of_sibling_inline_modules_in_one_file() -> Result<(), String> {
    let advice = rust_advice(&[(
        "src/lib.rs",
        "mod one {\n    pub(crate) fn f() {}\n}\nmod two {\n    pub(crate) fn g() {\n        super::one::f();\n    }\n}\nfn go() {\n    two::g();\n}\n",
    )])?;
    assert!(
        !mentions(&advice, "f") && !mentions(&advice, "g"),
        "{advice:?}"
    );
    Ok(())
}

#[test]
fn should_not_flag_a_type_named_only_by_a_trait_impl_signature() -> Result<(), String> {
    let advice = rust_advice(&[
        (
            "src/lib.rs",
            "mod a;\nmod b;\nfn go() {\n    let _ = a::X;\n}\n",
        ),
        (
            "src/b.rs",
            "pub(crate) trait Tr {\n    type A;\n    fn m(&self) -> Self::A;\n}\n",
        ),
        (
            "src/a.rs",
            "pub(crate) struct Helper;\npub(crate) struct X;\nimpl crate::b::Tr for X {\n    type A = Helper;\n    fn m(&self) -> Helper {\n        Helper\n    }\n}\n",
        ),
    ])?;
    assert!(!mentions(&advice, "Helper"), "{advice:?}");
    Ok(())
}

#[test]
fn should_not_flag_a_type_exposed_by_a_where_clause_field_alias_or_supertrait() -> Result<(), String>
{
    let cases = [
        "pub(crate) fn f<T>() where T: Into<Helper> {}\n",
        "pub(crate) struct S {\n    pub(crate) field: Helper,\n}\n",
        "pub(crate) type Alias = Helper;\n",
        "pub(crate) trait Sup: Into<Helper> {}\n",
    ];
    for case in cases {
        let body = format!("pub(crate) struct Helper;\n{case}");
        let advice = rust_advice(&[("src/lib.rs", "mod a;\n"), ("src/a.rs", &body)])?;
        // only the exposure rule keeps `Helper` from a private spelling.
        assert!(!mentions(&advice, "Helper"), "{case}: {advice:?}");
    }
    Ok(())
}

/// Analyzes a throwaway project with no Cargo manifest and cleans it up.
fn plain_advice(files: &[(&str, &str)]) -> Result<Vec<String>, String> {
    let root = write_project(files, false)?;
    let advice = advice_of(&root);
    let _ = fs::remove_dir_all(&root);
    advice
}

#[test]
fn should_stay_silent_for_typescript_exports_whether_used_across_folders_or_not()
-> Result<(), String> {
    // rule 5: no ladder, so only a file-local need is advised.
    let advice = plain_advice(&[
        (
            "src/a/x.ts",
            "export function shared(): number {\n  return 1;\n}\nexport function own(): number {\n  return 2;\n}\nexport function run(): number {\n  return own();\n}\n",
        ),
        (
            "src/b/y.ts",
            "import { run, shared } from \"../a/x\";\nexport const v: number = shared() + run();\n",
        ),
    ])?;
    assert!(!mentions(&advice, "shared"), "{advice:?}");
    assert!(!mentions(&advice, "run"), "{advice:?}");
    // these adapters declare every node at File, so a file-local export has
    // nothing narrower to advise; the flagged half of rule 5 is pinned at the
    // IR level (findings/tests.rs).
    assert!(!mentions(&advice, "own"), "{advice:?}");
    Ok(())
}

#[test]
fn should_stay_silent_for_python_names_whether_used_across_folders_or_not() -> Result<(), String> {
    let advice = plain_advice(&[
        (
            "a/x.py",
            "def shared() -> int:\n    return 1\n\n\ndef own() -> int:\n    return 2\n\n\ndef run() -> int:\n    return own()\n",
        ),
        (
            "b/y.py",
            "from a.x import run, shared\n\nvalue = shared() + run()\n",
        ),
        ("a/__init__.py", ""),
        ("b/__init__.py", ""),
    ])?;
    assert!(!mentions(&advice, "shared"), "{advice:?}");
    assert!(!mentions(&advice, "run"), "{advice:?}");
    Ok(())
}

/// A crate whose `tick` is `pub(crate)` yet used outside `a.rs` only by
/// `cfg(not(test))` code, which the semantic loader never sees (ADR-25).
const NOT_TEST_A: &str = "pub(crate) struct Helper;\nimpl Helper {\n    pub(crate) fn tick(&self) {}\n}\npub(crate) fn run() {\n    Helper.tick();\n}\n";

/// A crate file list that owns its contents.
type OwnedFiles = Vec<(&'static str, String)>;

/// The crate with `drive` in a `cfg(not(test))` module file; `body` is its body.
fn not_test_file_crate(body: &str) -> OwnedFiles {
    vec![
        (
            "src/lib.rs",
            "mod a;\n#[cfg(not(test))]\nmod real;\nfn go() {\n    a::run();\n}\n".to_owned(),
        ),
        ("src/a.rs", NOT_TEST_A.to_owned()),
        (
            "src/real.rs",
            format!("pub(crate) fn drive(h: &crate::a::Helper) {{\n    {body}\n}}\n"),
        ),
    ]
}

/// The crate with `drive` as an inline `cfg(not(test))` function in `lib.rs`.
fn not_test_inline_crate(body: &str) -> OwnedFiles {
    vec![
        (
            "src/lib.rs",
            format!(
                "mod a;\n#[cfg(not(test))]\nfn drive(h: &a::Helper) {{\n    {body}\n}}\nfn go() {{\n    a::run();\n}}\n"
            ),
        ),
        ("src/a.rs", NOT_TEST_A.to_owned()),
    ]
}

/// Borrows an owned file list.
fn borrowed(files: &OwnedFiles) -> Vec<(&str, &str)> {
    files
        .iter()
        .map(|(path, text)| (*path, text.as_str()))
        .collect()
}

#[test]
fn should_not_flag_an_item_used_only_by_a_cfg_not_test_module_file() -> Result<(), String> {
    let counted = rust_advice(&borrowed(&not_test_file_crate("h.tick();")))?;
    assert!(!mentions(&counted, "Helper::tick"), "{counted:?}");

    // control: the same file without the name leaves `tick` flagged.
    let control = rust_advice(&borrowed(&not_test_file_crate("let _ = h;")))?;
    assert!(mentions(&control, "Helper::tick"), "{control:?}");
    Ok(())
}

#[test]
fn should_not_flag_an_item_used_only_by_an_inline_cfg_not_test_function() -> Result<(), String> {
    let counted = rust_advice(&borrowed(&not_test_inline_crate("h.tick();")))?;
    assert!(!mentions(&counted, "Helper::tick"), "{counted:?}");

    let control = rust_advice(&borrowed(&not_test_inline_crate("let _ = h;")))?;
    assert!(mentions(&control, "Helper::tick"), "{control:?}");
    Ok(())
}

/// Removes every visibility finding and the source hash from an analysis.
fn strip_visibility(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(items) => {
            items.retain(|item| {
                item.get("kind").and_then(|kind| kind.as_str()) != Some("visibility")
            });
            items.iter_mut().for_each(strip_visibility);
        }
        serde_json::Value::Object(fields) => {
            fields.remove("snapshotHash");
            fields.values_mut().for_each(strip_visibility);
        }
        _ => {}
    }
}

/// The whole analysis of a crate except its visibility findings.
fn analysis_without_advice(files: &OwnedFiles) -> Result<String, String> {
    let root = write_project(&borrowed(files), true)?;
    let config = AnalyzeConfig::default();
    let json = snapshot_from_root(&root, &config)
        .map_err(|error| error.to_string())
        .and_then(|snapshot| analyze(&snapshot, &config).map_err(|error| error.to_string()))
        .and_then(|result| serde_json::to_value(&result).map_err(|e| e.to_string()));
    let _ = fs::remove_dir_all(&root);
    let name = root
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let mut json = json?;
    strip_visibility(&mut json);
    // the root name is not a result.
    serde_json::to_string_pretty(&json)
        .map(|text| text.replace(&name, "root"))
        .map_err(|error| error.to_string())
}

#[test]
fn should_leave_scores_and_moves_identical_when_a_cfg_not_test_name_counts_as_a_use()
-> Result<(), String> {
    // the names count for visibility advice only: with or without the use, no
    // score, cohesion, cycle or move may differ.
    for build in [not_test_file_crate, not_test_inline_crate] {
        let with_use = analysis_without_advice(&build("h.tick();"))?;
        let without_use = analysis_without_advice(&build("let _ = h;"))?;
        assert!(with_use == without_use, "the analysis differs");
    }
    Ok(())
}
