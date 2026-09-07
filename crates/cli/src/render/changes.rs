//! Physical affected paths derived from saved file identities and move records.

use std::collections::{BTreeMap, BTreeSet};

use strata_engine::{AnalyzeResult, Candidate, ContainerNode, Level, SymbolKind};

use super::wrap;

/// Before/after physical files, annotated only with relocated symbols.
pub(super) struct Changes {
    root: Option<String>,
    relocations: BTreeMap<String, String>,
    before: BTreeMap<String, Vec<String>>,
    after: BTreeMap<String, Vec<String>>,
}

impl Changes {
    pub(super) fn build(result: &AnalyzeResult, candidate: &Candidate) -> Self {
        let mut changes = Self::paths(result);
        for entry in &candidate.delta_narration {
            for file in &entry.files {
                changes.add_file(&file.path, &entry.to);
            }
            for mirror in &entry.mirrors {
                changes.add_file(&mirror.path, &mirror.to);
            }
        }
        for entry in &candidate.symbol_moves {
            let source = entry.from_path.clone();
            let destination = entry.to_path.clone();
            let after_source = changes.after_path(&entry.from_path);
            let after_destination = changes.after_path(&entry.to_path);
            // Both symbol-move files survive; a whole-file relocation may change their physical place.
            changes.before.entry(destination).or_default();
            changes.after.entry(after_source).or_default();
            let kind = if entry.kind == SymbolKind::Type {
                "type"
            } else {
                "symbol"
            };
            changes
                .before
                .entry(source)
                .or_default()
                .push(format!("{kind} `{}` [moves out]", entry.symbol));
            changes
                .after
                .entry(after_destination)
                .or_default()
                .push(format!("{kind} `{}` [moved in]", entry.symbol));
        }
        changes
    }

    pub(super) fn paths(result: &AnalyzeResult) -> Self {
        let mut roots = BTreeSet::new();
        collect_roots(&result.current.tree, &mut roots);
        let root = if roots.len() == 1 {
            roots.first().cloned()
        } else {
            None
        };
        Self {
            root,
            relocations: BTreeMap::new(),
            before: BTreeMap::new(),
            after: BTreeMap::new(),
        }
    }

    fn add_file(&mut self, path: &str, destination: &str) {
        let before = self.path(path);
        let after = self.destination(path, destination);
        if before != after {
            self.before.entry(before.clone()).or_default();
            self.after.entry(after.clone()).or_default();
            self.relocations.insert(before, after);
        }
    }

    pub(super) fn path(&self, path: &str) -> String {
        self.root
            .as_ref()
            .and_then(|root| path.strip_prefix(&format!("{root}/")))
            .unwrap_or(path)
            .to_owned()
    }

    pub(super) fn destination(&self, path: &str, folder: &str) -> String {
        let leaf = path.rsplit('/').next().unwrap_or(path);
        let is_root = self.root.as_deref() == Some(folder);
        let folder = self.path(folder);
        if folder.is_empty() || folder == "." || is_root {
            leaf.to_owned()
        } else {
            format!("{folder}/{leaf}")
        }
    }

    pub(super) fn after_path(&self, identity: &str) -> String {
        self.relocations
            .get(identity)
            .cloned()
            .unwrap_or_else(|| identity.to_owned())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.before.is_empty() && self.after.is_empty()
    }

    pub(super) fn impact_lines(&self) -> Vec<String> {
        let mut impacts: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for (before, after) in &self.relocations {
            impacts.entry(parent(before).to_owned()).or_default().0 += 1;
            impacts.entry(parent(after).to_owned()).or_default().1 += 1;
        }
        let mut lines = vec!["  Folder impacts".to_owned()];
        if impacts.is_empty() {
            lines.push("    No files enter or leave a folder.".to_owned());
        }
        for (folder, (leaving, entering)) in impacts {
            lines.extend(wrap(
                &format!("`{folder}`: {leaving} file(s) leaving · {entering} entering"),
                4,
                4,
            ));
        }
        lines
    }

    pub(super) fn tree_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (heading, files) in [("Before", &self.before), ("After", &self.after)] {
            lines.push(format!("  {heading}"));
            if files.is_empty() {
                lines.push("    No affected branches.".to_owned());
                continue;
            }
            let mut tree = Branch::default();
            for (path, symbols) in files {
                tree.insert(path, symbols);
            }
            lines.push(format!("    {}/", self.root.as_deref().unwrap_or(".")));
            tree.draw("    ", &mut lines);
        }
        lines.push("  * File moved or its symbol contents changed.".to_owned());
        lines.push(
            "  Unchanged branches and symbols omitted; excluded files are not shown.".to_owned(),
        );
        lines
    }
}

fn collect_roots(node: &ContainerNode, roots: &mut BTreeSet<String>) {
    if node.level == Level::Package {
        roots.insert(node.name.clone());
        return;
    }
    for child in node.children.iter().flatten() {
        collect_roots(child, roots);
    }
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or(".", |(folder, _)| folder)
}

#[derive(Default)]
struct Branch {
    children: BTreeMap<String, Branch>,
    symbols: Option<Vec<String>>,
}

impl Branch {
    fn insert(&mut self, path: &str, symbols: &[String]) {
        let mut branch = self;
        for component in path.split('/') {
            branch = branch.children.entry(component.to_owned()).or_default();
        }
        let mut symbols = symbols.to_vec();
        symbols.sort();
        symbols.dedup();
        branch.symbols = Some(symbols);
    }

    fn draw(&self, prefix: &str, lines: &mut Vec<String>) {
        for (index, (name, branch)) in self.children.iter().enumerate() {
            let last = index + 1 == self.children.len();
            let connector = if last { "└── " } else { "├── " };
            let suffix = if branch.symbols.is_some() { " *" } else { "/" };
            lines.push(format!("{prefix}{connector}{name}{suffix}"));
            let continuation = format!("{prefix}{}", if last { "    " } else { "│   " });
            if let Some(symbols) = &branch.symbols {
                for (symbol_index, symbol) in symbols.iter().enumerate() {
                    let symbol_connector = if symbol_index + 1 == symbols.len() {
                        "└── "
                    } else {
                        "├── "
                    };
                    lines.push(format!("{continuation}{symbol_connector}{symbol}"));
                }
            }
            branch.draw(&continuation, lines);
        }
    }
}
