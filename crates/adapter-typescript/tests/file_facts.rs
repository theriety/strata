//! TypeScript test-pair facts: the adapter, not the engine, knows the
//! `foo.ts` / `foo.spec.ts` / `__tests__` conventions.

use smol_str::SmolStr;
use unlace_adapter_typescript::TypeScriptAdapter;
use unlace_ir::{Adapter, FileFact, SourceFile};

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

fn companion(file: &str, folder: &str) -> FileFact {
    FileFact::folder_companion(file, folder)
}

#[test]
fn should_state_a_module_beside_its_folder_as_the_folders_companion() {
    assert_eq!(
        facts(&["src/block.ts", "src/block/leaf.ts", "src/other.ts"]),
        vec![companion("src/block.ts", "src/block")]
    );
}

#[test]
fn should_state_a_root_level_companion_for_every_module_extension() {
    assert_eq!(
        facts(&[
            "a.ts",
            "a/x.ts",
            "src/b.tsx",
            "src/b/x.ts",
            "src/c.mts",
            "src/c/x.ts",
        ]),
        vec![
            companion("a.ts", "a"),
            companion("src/b.tsx", "src/b"),
            companion("src/c.mts", "src/c"),
        ]
    );
}

#[test]
fn should_state_nothing_when_the_folder_has_its_own_index() {
    assert_eq!(
        facts(&["src/block.ts", "src/block/index.ts", "src/block/leaf.ts"]),
        Vec::<FileFact>::new()
    );
}

#[test]
fn should_state_nothing_when_two_modules_share_the_stem() {
    assert_eq!(
        facts(&["src/block.ts", "src/block.tsx", "src/block/leaf.ts"]),
        Vec::<FileFact>::new()
    );
}

#[test]
fn should_state_nothing_when_the_folder_holds_no_analysed_module() {
    assert_eq!(
        facts(&["src/block.ts", "src/block/logo.svg", "src/other/leaf.ts"]),
        Vec::<FileFact>::new()
    );
}

#[test]
fn should_give_a_paired_spec_its_mirrored_companion_folder() {
    assert_eq!(
        facts(&[
            "src/lexer/block.ts",
            "src/lexer/block/leaf.ts",
            "spec/lexer/block.spec.ts",
            "spec/lexer/block/leaf.spec.ts",
        ]),
        vec![
            FileFact {
                folder_companion: Some(SmolStr::new("spec/lexer/block")),
                ..pair("spec/lexer/block.spec.ts", "src/lexer/block.ts")
            },
            pair("spec/lexer/block/leaf.spec.ts", "src/lexer/block/leaf.ts"),
            companion("src/lexer/block.ts", "src/lexer/block"),
        ]
    );
}

#[test]
fn should_give_a_paired_spec_no_companion_when_its_mirror_folder_is_absent() {
    assert_eq!(
        facts(&[
            "src/lexer/block.ts",
            "src/lexer/block/leaf.ts",
            "spec/lexer/block.spec.ts",
        ]),
        vec![
            pair("spec/lexer/block.spec.ts", "src/lexer/block.ts"),
            companion("src/lexer/block.ts", "src/lexer/block"),
        ]
    );
}

#[test]
fn should_state_a_companion_at_every_nesting_level() {
    assert_eq!(
        facts(&[
            "src/block.ts",
            "src/block/mdcx.ts",
            "src/block/mdcx/body.ts"
        ]),
        vec![
            companion("src/block.ts", "src/block"),
            companion("src/block/mdcx.ts", "src/block/mdcx"),
        ]
    );
}
