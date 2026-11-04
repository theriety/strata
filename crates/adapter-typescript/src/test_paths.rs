//! Which TypeScript paths are test code: name conventions and test roots.
//!
//! A file is test code when its name says so (`*.spec.*`, `*.test.*`, a
//! `__tests__` folder) or when its first path segment below any enclosing
//! package root (a `package.json` directory, or the analysed root) is a test
//! root (`spec`, `test`, `tests`), so support files such as
//! `spec/batch/utilities.ts` count too, and so does a fixture package nested
//! inside such a root.
//!
//! The role is decided the same way for every source: only a test-named file
//! (or one in a `__tests__` folder) is a test case, and every other test file
//! is test support.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use smol_str::SmolStr;

use crate::parse::ParsedModule;

/// Directory names that root a package's tests; also the mirror roots of test pairs.
pub(crate) const TEST_ROOTS: [&str; 3] = ["spec", "test", "tests"];

/// The extensions of the TypeScript-family sources the adapter reasons about.
pub(crate) const EXTENSIONS: [&str; 8] = ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

/// The directories of `modules` (repo-relative, `/`-separated) that hold a
/// `package.json` on disk under `root`; the analysed root itself is implicit.
pub(crate) fn package_roots(root: &Path, modules: &[ParsedModule]) -> BTreeSet<SmolStr> {
    let mut known: BTreeMap<&str, bool> = BTreeMap::new();
    for module in modules {
        let mut directory = module.path.rsplit_once('/').map(|(parent, _)| parent);
        while let Some(current) = directory {
            known
                .entry(current)
                .or_insert_with(|| root.join(current).join("package.json").is_file());
            directory = current.rsplit_once('/').map(|(parent, _)| parent);
        }
    }
    known
        .into_iter()
        .filter(|&(_, has_manifest)| has_manifest)
        .map(|(directory, _)| SmolStr::new(directory))
        .collect()
}

/// Returns `true` if `path` is test code by name or by test root.
pub(crate) fn is_test_path(path: &str, package_roots: &BTreeSet<SmolStr>) -> bool {
    is_test_case_path(path) || is_under_test_root(path, package_roots)
}

/// Returns `true` if the first segment of `path` below any enclosing package
/// root (the analysed root included) is a test root. A package nested inside an
/// outer package's test root, such as a fixture package, is test code too.
fn is_under_test_root(path: &str, package_roots: &BTreeSet<SmolStr>) -> bool {
    let below_test_root = |relative: &str| {
        relative
            .split_once('/')
            .is_some_and(|(first, _)| TEST_ROOTS.contains(&first))
    };
    below_test_root(path)
        || package_roots.iter().any(|package| {
            path.strip_prefix(package.as_str())
                .and_then(|rest| rest.strip_prefix('/'))
                .is_some_and(below_test_root)
        })
}

/// Returns `true` if `path` is a test case: a file in a `__tests__` folder or
/// one whose name carries a `.spec.` or `.test.` qualifier, in any
/// TypeScript-family extension. Every other test file, such as a helper under a
/// test root, is test support.
pub(crate) fn is_test_case_path(path: &str) -> bool {
    if path.split('/').any(|segment| segment == "__tests__") {
        return true;
    }

    let Some((stem, extension)) = path.rsplit_once('.') else {
        return false;
    };
    if !EXTENSIONS.contains(&extension) {
        return false;
    }
    let file_name = stem.rsplit('/').next().unwrap_or(stem);

    file_name
        .split('.')
        .skip(1)
        .any(|segment| matches!(segment, "spec" | "test"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(paths: &[&str]) -> BTreeSet<SmolStr> {
        paths.iter().copied().map(SmolStr::new).collect()
    }

    #[test]
    fn should_recognise_test_paths_by_convention() {
        let none = roots(&[]);
        assert!(is_test_path("src/__tests__/app.spec.ts", &none));
        assert!(is_test_path("src/app.test.ts", &none));
        assert!(is_test_path("src/__tests__/support.ts", &none));
        assert!(!is_test_path("src/app.ts", &none));
    }

    #[test]
    fn should_recognize_qualified_test_paths_without_matching_words() {
        let none = roots(&[]);
        assert!(is_test_path("src/worker.spec.int.ts", &none));
        assert!(is_test_path("src/worker.test.integration.ts", &none));
        assert!(is_test_path("src/worker.spec.browser.tsx", &none));
        assert!(!is_test_path("src/specification.ts", &none));
        assert!(!is_test_path("src/worker.testable.tsx", &none));
    }

    #[test]
    fn should_recognise_test_names_in_every_typescript_family_extension() {
        let none = roots(&[]);
        for path in [
            "src/a.spec.mts",
            "src/a.test.mts",
            "src/a.spec.cts",
            "src/a.test.cts",
            "src/a.spec.tsx",
            "src/a.test.js",
            "src/a.spec.mjs",
            "src/a.test.jsx",
        ] {
            assert!(is_test_path(path, &none), "{path} is test code by name");
            assert!(is_test_case_path(path), "{path} is a test case");
        }
        assert!(!is_test_path("src/specification.mts", &none));
        assert!(!is_test_path("src/a.testable.cts", &none));
        assert!(!is_test_path("src/a.spec.json", &none));
    }

    #[test]
    fn should_carry_test_case_only_for_test_named_files_and_tests_folders() {
        assert!(is_test_case_path("src/a.spec.ts"));
        assert!(is_test_case_path("src/__tests__/support.ts"));
        assert!(
            !is_test_case_path("spec/util.ts"),
            "a helper in a test root is support, not a case"
        );
        assert!(!is_test_case_path("test/helpers/h.ts"));
        assert!(!is_test_case_path("src/app.ts"));
    }

    #[test]
    fn should_treat_a_test_root_helper_as_test_code() {
        let none = roots(&[]);
        assert!(is_test_path("spec/batch/utilities.ts", &none));
        assert!(is_test_path("test/helpers.ts", &none));
        assert!(is_test_path("tests/a/b.ts", &none));
        assert!(!is_test_path("src/spec/helpers.ts", &none));
        assert!(!is_test_path("specs/helpers.ts", &none));
        assert!(!is_test_path("spec.ts", &none));
    }

    #[test]
    fn should_measure_test_roots_from_an_enclosing_package_root() {
        let packages = roots(&["packages/foo"]);
        assert!(is_test_path("packages/foo/spec/h.ts", &packages));
        assert!(!is_test_path("packages/foo/src/h.ts", &packages));
        assert!(!is_test_path("packages/bar/src/h.ts", &packages));
        assert!(!is_test_path("packages/foo/src/spec/h.ts", &packages));
        // without a manifest `packages/` is no root, so `spec` is not first.
        assert!(!is_test_path("packages/foo/spec/h.ts", &roots(&[])));
    }

    #[test]
    fn should_let_every_enclosing_package_root_decide() {
        let nested = roots(&["packages", "packages/foo"]);
        assert!(is_test_path("packages/foo/spec/h.ts", &nested));
        assert!(is_test_path("packages/spec/h.ts", &nested));
        assert!(is_test_path("spec/h.ts", &nested));
        assert!(!is_test_path("packages/foo/src/h.ts", &nested));
    }

    #[test]
    fn should_not_enclose_a_sibling_that_shares_a_name_prefix() {
        let foo = roots(&["packages/foo"]);
        assert!(!is_test_path("packages/foobar/spec/h.ts", &foo));
        assert!(is_test_path(
            "packages/foobar/spec/h.ts",
            &roots(&["packages/foo", "packages/foobar"])
        ));
    }

    #[test]
    fn should_let_a_fixture_package_inherit_test_status_from_an_outer_test_root() {
        let fixture = roots(&["spec/fixtures/pkg"]);
        assert!(is_test_path("spec/fixtures/pkg/h.ts", &fixture));
        assert!(is_test_path("spec/fixtures/pkg/src/h.ts", &fixture));
        let production = roots(&["fixtures/pkg"]);
        assert!(!is_test_path("fixtures/pkg/src/h.ts", &production));
        assert!(is_test_path("fixtures/pkg/spec/h.ts", &production));
    }

    fn module(path: &str) -> ParsedModule {
        ParsedModule {
            path: SmolStr::new(path),
            declarations: Vec::new(),
            imports: Vec::new(),
            re_exports: Vec::new(),
        }
    }

    /// The package roots found for `modules` under a throwaway directory that
    /// holds a `package.json` in each of `manifest_dirs` (`""` is the root).
    fn found_roots(
        name: &str,
        manifest_dirs: &[&str],
        modules: &[&str],
    ) -> Result<BTreeSet<SmolStr>, std::io::Error> {
        let dir = std::env::temp_dir().join(format!("unlace-ts-{name}-{}", std::process::id()));
        for manifest_dir in manifest_dirs {
            std::fs::create_dir_all(dir.join(manifest_dir))?;
            std::fs::write(dir.join(manifest_dir).join("package.json"), "{}")?;
        }
        let parsed: Vec<ParsedModule> = modules.iter().map(|path| module(path)).collect();
        let found = package_roots(&dir, &parsed);
        std::fs::remove_dir_all(&dir)?;
        Ok(found)
    }

    #[test]
    fn should_find_package_roots_that_have_a_manifest_on_disk() -> Result<(), std::io::Error> {
        let found = found_roots(
            "roots",
            &["packages/foo"],
            &["packages/foo/spec/h.ts", "packages/bar/a.ts"],
        )?;
        assert_eq!(found, roots(&["packages/foo"]));
        Ok(())
    }

    #[test]
    fn should_keep_the_analysed_root_implicit_when_it_holds_a_manifest()
    -> Result<(), std::io::Error> {
        let found = found_roots(
            "root-manifest",
            &["", "packages/foo"],
            &["spec/h.ts", "packages/foo/spec/h.ts", "src/spec/h.ts"],
        )?;
        assert_eq!(found, roots(&["packages/foo"]));
        assert!(is_test_path("spec/h.ts", &found));
        assert!(is_test_path("packages/foo/spec/h.ts", &found));
        assert!(!is_test_path("src/spec/h.ts", &found));
        Ok(())
    }
}
