//! Python test-pair and entry facts: the adapter, not the engine, knows the
//! `test_foo.py` / `foo_test.py` conventions.

use smol_str::SmolStr;
use strata_adapter_python::PythonAdapter;
use strata_ir::{Adapter, FileFact, SourceFile};

fn facts(paths: &[&str]) -> Vec<FileFact> {
    let adapter = PythonAdapter::default();
    let files: Vec<SourceFile> = paths
        .iter()
        .map(|path| SourceFile {
            path: SmolStr::new(*path),
            contents: "X = 1\n".to_owned(),
        })
        .collect();
    let trees = adapter.parse(&files).unwrap_or_default();
    adapter.bind(trees).unwrap_or_default().file_facts
}

#[test]
fn should_pair_test_prefixed_and_suffixed_files_under_both_layouts() {
    assert_eq!(
        facts(&[
            "src/pkg/foo.py",
            "tests/pkg/test_foo.py",
            "src/pkg/bar.py",
            "src/pkg/bar_test.py",
            "src/baz.py",
            "test/baz_test.py",
        ]),
        vec![
            FileFact::test_pair("src/pkg/bar_test.py", "src/pkg/bar.py"),
            FileFact::test_pair("test/baz_test.py", "src/baz.py"),
            FileFact::test_pair("tests/pkg/test_foo.py", "src/pkg/foo.py"),
        ]
    );
}

#[test]
fn should_not_pair_without_a_present_mirror_or_for_conftest() {
    assert_eq!(
        facts(&["src/pkg/foo.py", "tests/conftest.py", "tests/test_other.py"]),
        Vec::<FileFact>::new()
    );
}

#[test]
fn should_record_main_and_setup_as_entries_but_not_conftest() {
    assert_eq!(
        facts(&[
            "setup.py",
            "src/pkg/__main__.py",
            "src/pkg/util.py",
            "conftest.py"
        ]),
        vec![
            FileFact::entry_file("setup.py"),
            FileFact::entry_file("src/pkg/__main__.py"),
        ]
    );
}

#[test]
fn should_not_treat_a_nested_setup_as_an_entry() {
    assert_eq!(facts(&["docs/setup.py"]), Vec::<FileFact>::new());
}
