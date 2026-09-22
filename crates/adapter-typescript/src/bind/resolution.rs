//! Module, import, and exported-name resolution.

use std::collections::{BTreeMap, HashMap, HashSet};

use smol_str::SmolStr;
use strata_ir::NodeId;

use crate::parse::ParsedModule;

use super::ExportTable;

/// Maps each locally imported name to its resolved target node and type-only flag.
pub(super) fn resolve_imports(
    module: &ParsedModule,
    resolver: &Resolver,
    exports: &ExportTable,
) -> HashMap<SmolStr, (NodeId, bool)> {
    let mut imported: HashMap<SmolStr, (NodeId, bool)> = HashMap::new();
    for import in &module.imports {
        let Some(target_module) = resolver.resolve(&module.path, &import.source) else {
            continue;
        };
        let Some(target_exports) = exports.get(&target_module) else {
            continue;
        };
        for name in &import.names {
            if let Some(&target) = target_exports.get(name) {
                imported.insert(name.clone(), (target, import.type_only));
            }
        }
    }
    imported
}

/// Resolves module specifiers to canonical module paths.
pub(super) struct Resolver {
    /// Set of known module paths, used to verify a resolution target exists.
    known: HashSet<SmolStr>,
    /// `tsconfig`-style alias prefix -> target path prefix mappings.
    aliases: BTreeMap<SmolStr, SmolStr>,
    /// Node.js subpath-import specifier -> target mappings (`package.json`).
    imports: BTreeMap<SmolStr, SmolStr>,
}

impl Resolver {
    /// Builds a resolver over the known module set, `tsconfig` aliases, and
    /// Node.js subpath imports.
    pub(super) fn new(
        modules: &[ParsedModule],
        aliases: BTreeMap<SmolStr, SmolStr>,
        imports: BTreeMap<SmolStr, SmolStr>,
    ) -> Self {
        Self {
            known: modules.iter().map(|module| module.path.clone()).collect(),
            aliases,
            imports,
        }
    }

    /// Resolves `specifier` imported from `importer` to a known module path.
    ///
    /// Order: relative path -> Node.js subpath import -> alias prefix ->
    /// package entry (`index`).
    pub(super) fn resolve(&self, importer: &str, specifier: &str) -> Option<SmolStr> {
        if specifier.starts_with('.') {
            return self.resolve_relative(importer, specifier);
        }
        if specifier.starts_with('#') && !self.imports.is_empty() {
            return self.resolve_subpath_import(specifier);
        }
        if let Some(resolved) = self.resolve_alias(specifier) {
            return Some(resolved);
        }
        self.resolve_package(specifier)
    }

    /// Resolves a relative specifier against the importer's directory.
    fn resolve_relative(&self, importer: &str, specifier: &str) -> Option<SmolStr> {
        let importer_dir = importer.rsplit_once('/').map_or("", |(dir, _)| dir);
        let joined = normalize_join(importer_dir, specifier);
        self.with_extensions(&joined)
    }

    /// Resolves a specifier through the `tsconfig` `paths` aliases.
    fn resolve_alias(&self, specifier: &str) -> Option<SmolStr> {
        for (alias, target) in &self.aliases {
            if let Some(rest) = specifier.strip_prefix(alias.as_str()) {
                let candidate = format!("{target}{rest}");
                if let Some(resolved) = self.with_extensions(&candidate) {
                    return Some(resolved);
                }
            }
        }
        None
    }

    /// Resolves a bare package specifier to its entry module, if it maps to a
    /// known in-repo module (workspace package) rather than an external dep.
    fn resolve_package(&self, specifier: &str) -> Option<SmolStr> {
        let base = format!("{specifier}/src/index");
        self.with_extensions(&base)
            .or_else(|| self.with_extensions(&format!("{specifier}/index")))
    }

    /// Resolves a Node.js subpath import (`#agent/schemas`) through the root
    /// `package.json` `imports` map. Exact keys win over single-star patterns;
    /// the matched pattern's `*` substitutes once into the target's own `*`.
    fn resolve_subpath_import(&self, specifier: &str) -> Option<SmolStr> {
        if let Some(direct) = self.imports.get(specifier) {
            return self.resolve_import_target(direct);
        }
        for (pattern, target) in &self.imports {
            let Some((prefix, suffix)) = pattern.split_once('*') else {
                continue;
            };
            let Some(rest) = specifier.strip_prefix(prefix) else {
                continue;
            };
            let Some(rest) = rest.strip_suffix(suffix) else {
                continue;
            };
            if rest.contains('*') {
                continue;
            }
            return self.resolve_import_target(&target.replace('*', rest));
        }
        None
    }

    /// Resolves an `imports` target against the repository root: literal known
    /// paths win, otherwise extension and index-barrel forms apply.
    fn resolve_import_target(&self, target: &str) -> Option<SmolStr> {
        let trimmed = target.trim_start_matches("./");
        let literal = SmolStr::new(trimmed);
        if self.known.contains(&literal) {
            return Some(literal);
        }
        self.with_extensions(trimmed)
    }

    /// Tries the candidate path with each TypeScript extension and `index` form.
    fn with_extensions(&self, base: &str) -> Option<SmolStr> {
        for extension in [".ts", ".tsx", ".d.ts"] {
            let candidate = SmolStr::new(format!("{base}{extension}"));
            if self.known.contains(&candidate) {
                return Some(candidate);
            }
        }
        for index in ["/index.ts", "/index.tsx"] {
            let candidate = SmolStr::new(format!("{base}{index}"));
            if self.known.contains(&candidate) {
                return Some(candidate);
            }
        }
        None
    }
}

/// Joins `dir` and a relative `specifier`, collapsing `.` and `..` segments.
pub(super) fn normalize_join(dir: &str, specifier: &str) -> String {
    let mut segments: Vec<&str> = if dir.is_empty() {
        Vec::new()
    } else {
        dir.split('/').collect()
    };
    for segment in specifier.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    segments.join("/")
}
