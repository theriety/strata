//! TypeScript test-pair facts: the adapter, not the engine, knows the
//! `foo.ts` / `foo.spec.ts` / `__tests__` conventions.

use smol_str::SmolStr;
use strata_adapter_typescript::TypeScriptAdapter;
use strata_ir::{Adapter, FileFact, SourceFile};

fn facts(paths: &[&str]) -> Vec<FileFact> {
    let adapter = TypeScriptAdapter::default();
    let files: Vec<SourceFile> = paths
        .iter()
        .map(|path| SourceFile {
            path: SmolStr::new(*path),
            contents: "export const x = 1;\n".to_owned(),
        })
        .collect();
    let trees = adapter.parse(&files).unwrap_or_default();
    adapter.bind(trees).unwrap_or_default().file_facts
}

fn pair(test: &str, source: &str) -> FileFact {
    FileFact::test_pair(test, source)
}

#[test]
fn should_pair_a_sibling_spec_with_its_source() {
    assert_eq!(
        facts(&["src/a/foo.ts", "src/a/foo.spec.ts"]),
        vec![pair("src/a/foo.spec.ts", "src/a/foo.ts")]
    );
}

#[test]
fn should_pair_mirrored_test_roots_for_every_marker() {
    assert_eq!(
        facts(&[
            "src/a/foo.ts",
            "test/a/foo.test.ts",
            "src/a/bar.ts",
            "spec/a/bar.spec.ts",
            "src/baz.tsx",
            "tests/baz.test.tsx",
        ]),
        vec![
            pair("spec/a/bar.spec.ts", "src/a/bar.ts"),
            pair("test/a/foo.test.ts", "src/a/foo.ts"),
            pair("tests/baz.test.tsx", "src/baz.tsx"),
        ]
    );
}

#[test]
fn should_pair_under_the_source_root_spelling() {
    assert_eq!(
        facts(&["source/lib/x.js", "test/lib/x.spec.js"]),
        vec![pair("test/lib/x.spec.js", "source/lib/x.js")]
    );
}

#[test]
fn should_not_pair_when_the_extension_differs_or_no_test_is_present() {
    assert_eq!(
        facts(&["src/a.ts", "test/a.spec.js", "src/b.ts", "src/c.ts"]),
        Vec::<FileFact>::new()
    );
}

#[test]
fn should_pair_a_jest_tests_folder_with_the_source_beside_it() {
    assert_eq!(
        facts(&[
            "src/a/foo.ts",
            "src/a/__tests__/foo.test.ts",
            "src/b.tsx",
            "src/__tests__/b.spec.tsx",
            "src/c.ts",
            "src/other/__tests__/c.test.ts",
        ]),
        vec![
            pair("src/__tests__/b.spec.tsx", "src/b.tsx"),
            pair("src/a/__tests__/foo.test.ts", "src/a/foo.ts"),
        ]
    );
}

#[test]
fn should_not_treat_a_test_file_as_a_source() {
    assert_eq!(
        facts(&["src/foo.spec.ts", "test/foo.spec.spec.ts"]),
        Vec::<FileFact>::new()
    );
}

#[test]
fn should_record_no_entry_files() {
    let facts = facts(&["src/index.ts", "src/main.ts"]);
    assert_eq!(facts.iter().filter(|fact| fact.entry).count(), 0);
}
