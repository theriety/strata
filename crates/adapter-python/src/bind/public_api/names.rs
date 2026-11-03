//! Module naming for public-API detection only.
//!
//! Dependency edges treat a stub (`.pyi`) as an unrelated file and resolve only
//! what the strict [`Resolver`](super::super::resolver::Resolver) names. Public
//! API is about what callers can import, and a caller imports a module whether
//! it is shipped as a source, a stub, or both, so this index names every module
//! of the tree by its dotted path, a source and its stub under one name. It
//! never feeds an edge.
//!
//! A name that matches no module as written falls back to every module whose
//! dotted name ends with it (a `src/` layout writes `import pkg.mod` while the
//! tree knows `src.pkg.mod`), and when the analysed folder is itself a package,
//! its own name leads the dotted path. Every candidate is returned: a wrong
//! extra candidate only makes more public.

use std::collections::HashMap;

use smol_str::SmolStr;

use crate::parse::{Import, ParsedModule};

/// Splits a repository-relative path into its directory and file name.
pub(super) fn split_path(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

/// The module name of a source or stub file name.
pub(super) fn stem_of(file: &str) -> &str {
    file.strip_suffix(".pyi")
        .or_else(|| file.strip_suffix(".py"))
        .unwrap_or(file)
}

/// Whether `path` is a package initializer, source or stub.
fn is_init(path: &str) -> bool {
    stem_of(split_path(path).1) == "__init__"
}

/// The dotted name of a source or stub file; a root initializer is `""`.
fn dotted_name(path: &str) -> String {
    let (directory, file) = split_path(path);
    let stem = stem_of(file);
    let dotted = if stem == "__init__" {
        directory.to_owned()
    } else if directory.is_empty() {
        stem.to_owned()
    } else {
        format!("{directory}/{stem}")
    };
    dotted.replace('/', ".")
}

/// Every module of the tree under its dotted name.
pub(super) struct ModuleNames {
    /// Source file path -> its dotted name.
    dotted_of: HashMap<SmolStr, String>,
    /// Dotted name -> every file that carries it (source and stub), sorted.
    by_dotted: HashMap<String, Vec<SmolStr>>,
    /// Every trailing run of dotted segments -> the files whose dotted name ends
    /// with it, sorted.
    by_suffix: HashMap<String, Vec<SmolStr>>,
}

impl ModuleNames {
    /// Names the modules of the tree.
    pub(super) fn new(modules: &[ParsedModule]) -> Self {
        let mut dotted_of = HashMap::new();
        let mut by_dotted: HashMap<String, Vec<SmolStr>> = HashMap::new();
        let mut by_suffix: HashMap<String, Vec<SmolStr>> = HashMap::new();
        for module in modules {
            let dotted = dotted_name(&module.path);
            by_dotted
                .entry(dotted.clone())
                .or_default()
                .push(module.path.clone());
            if !dotted.is_empty() {
                let mut rest = dotted.as_str();
                loop {
                    by_suffix
                        .entry(rest.to_owned())
                        .or_default()
                        .push(module.path.clone());
                    match rest.split_once('.') {
                        Some((_, tail)) => rest = tail,
                        None => break,
                    }
                }
            }
            dotted_of.insert(module.path.clone(), dotted);
        }
        for paths in by_dotted.values_mut().chain(by_suffix.values_mut()) {
            paths.sort();
        }
        Self {
            dotted_of,
            by_dotted,
            by_suffix,
        }
    }

    /// The modules `from <module> import ...` draws from; none for a plain
    /// `import`.
    pub(super) fn sources(&self, importer: &str, import: &Import) -> Vec<SmolStr> {
        if import.level == 0 && import.module.is_empty() {
            return Vec::new();
        }
        self.package_dotted(importer, import)
            .map(|dotted| self.named(&dotted, import.level == 0))
            .unwrap_or_default()
    }

    /// The modules the name `child` could stand for in `from <module> import
    /// child`: submodules of that package, whether or not it has an
    /// `__init__.py`.
    pub(super) fn children(&self, importer: &str, import: &Import, child: &str) -> Vec<SmolStr> {
        let Some(package) = self.package_dotted(importer, import) else {
            return Vec::new();
        };
        let dotted = if package.is_empty() {
            child.to_owned()
        } else {
            format!("{package}.{child}")
        };
        self.named(&dotted, import.level == 0)
    }

    /// The modules an absolute dotted name could mean.
    pub(super) fn absolute(&self, dotted: &str) -> Vec<SmolStr> {
        self.named(dotted, true)
    }

    /// The dotted name of the package a `from` import draws from.
    fn package_dotted(&self, importer: &str, import: &Import) -> Option<String> {
        if import.level == 0 {
            return Some(import.module.to_string());
        }
        let mut segments: Vec<&str> = self
            .dotted_of
            .get(importer)?
            .split('.')
            .filter(|segment| !segment.is_empty())
            .collect();
        if !is_init(importer) {
            segments.pop();
        }
        for _ in 1..import.level {
            segments.pop()?;
        }
        segments.extend(
            import
                .module
                .split('.')
                .filter(|segment| !segment.is_empty()),
        );
        Some(segments.join("."))
    }

    /// The files named `dotted`; with `loose`, the files it could name when no
    /// module carries it exactly.
    fn named(&self, dotted: &str, loose: bool) -> Vec<SmolStr> {
        if let Some(paths) = self.by_dotted.get(dotted) {
            return paths.clone();
        }
        if !loose {
            return Vec::new();
        }
        let mut found = self.by_suffix.get(dotted).cloned().unwrap_or_default();
        if let Some(root) = self.by_dotted.get("") {
            match dotted.split_once('.') {
                Some((_, rest)) => {
                    found.extend(self.by_dotted.get(rest).into_iter().flatten().cloned());
                }
                None => found.extend(root.iter().cloned()),
            }
        }
        found.sort();
        found.dedup();
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::DunderAllForm;

    fn module_at(path: &str) -> ParsedModule {
        ParsedModule {
            path: SmolStr::new(path),
            declarations: Vec::new(),
            imports: Vec::new(),
            conditional_imports: Vec::new(),
            lazy_loading: false,
            dunder_all: Vec::new(),
            dunder_all_form: DunderAllForm::Absent,
        }
    }

    fn names(paths: &[&str]) -> ModuleNames {
        let modules: Vec<ParsedModule> = paths.iter().map(|path| module_at(path)).collect();
        ModuleNames::new(&modules)
    }

    fn from_import(level: u32, module: &str) -> Import {
        Import {
            module: SmolStr::new(module),
            level,
            names: vec![SmolStr::new("x")],
            targets: vec![SmolStr::new("x")],
            star: false,
            aliases: Vec::new(),
        }
    }

    #[test]
    fn should_name_a_source_and_its_stub_alike() {
        let index = names(&[
            "pkg/mod.py",
            "pkg/mod.pyi",
            "pkg/__init__.pyi",
            "__init__.py",
        ]);

        assert_eq!(
            index.absolute("pkg.mod"),
            [SmolStr::new("pkg/mod.py"), SmolStr::new("pkg/mod.pyi")]
        );
        assert_eq!(index.absolute("pkg"), [SmolStr::new("pkg/__init__.pyi")]);
        assert_eq!(
            index.dotted_of.get("__init__.py").map(String::as_str),
            Some("")
        );
    }

    #[test]
    fn should_resolve_a_relative_import_inside_a_namespace_package() {
        let index = names(&["ns/api.py", "ns/_impl.py", "top.py", "_other.py"]);

        assert_eq!(
            index.children("ns/api.py", &from_import(1, ""), "_impl"),
            [SmolStr::new("ns/_impl.py")]
        );
        assert_eq!(
            index.children("top.py", &from_import(1, ""), "_other"),
            [SmolStr::new("_other.py")]
        );
        assert_eq!(
            index.sources("ns/api.py", &from_import(1, "")),
            Vec::<SmolStr>::new()
        );
    }

    #[test]
    fn should_fall_back_to_every_module_whose_dotted_name_ends_with_the_import() {
        let index = names(&[
            "src/a/pkg/_impl.py",
            "src/b/pkg/_impl.py",
            "src/b/other/_impl.py",
        ]);

        assert_eq!(
            index.sources("app.py", &from_import(0, "pkg._impl")),
            [
                SmolStr::new("src/a/pkg/_impl.py"),
                SmolStr::new("src/b/pkg/_impl.py")
            ]
        );
        assert_eq!(
            index.sources("app.py", &from_import(0, "pkg.missing")),
            Vec::<SmolStr>::new()
        );
    }

    #[test]
    fn should_let_an_absolute_import_lead_with_the_name_of_a_root_package() {
        let index = names(&["__init__.py", "_impl.py"]);

        assert_eq!(
            index.sources("api.py", &from_import(0, "pkg._impl")),
            [SmolStr::new("_impl.py")]
        );
        assert_eq!(
            index.sources("api.py", &from_import(0, "pkg")),
            [SmolStr::new("__init__.py")]
        );
    }

    #[test]
    fn should_name_no_module_for_a_plain_import() {
        let index = names(&["pkg/mod.py"]);
        let plain = Import {
            module: SmolStr::new(""),
            ..from_import(0, "")
        };

        assert_eq!(index.sources("app.py", &plain), Vec::<SmolStr>::new());
    }
}
