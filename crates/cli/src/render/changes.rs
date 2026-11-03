//! Physical affected paths derived from saved file identities and move records.

use std::collections::{BTreeMap, BTreeSet};

use unlace_engine::{AnalyzeResult, Candidate, ContainerNode, Level, SymbolKind};

use super::wrap;

/// Before/after physical files, annotated only with relocated symbols.
pub(super) struct Changes {
    /// The label over the Before/After trees: the analyzed project's root, or `.`.
    heading: String,
    relocations: BTreeMap<String, String>,
    before: BTreeMap<String, Vec<String>>,
    after: BTreeMap<String, Vec<String>>,
}

impl Changes {
    pub(super) fn build(result: &AnalyzeResult, candidate: &Candidate) -> Self {
        let mut changes = Self::new(result);
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
            let outgoing = format!("{kind} `{}` [to {after_destination}]", entry.label());
            let incoming = format!("{kind} `{}` [from {source}]", entry.label());
            changes.before.entry(source).or_default().push(outgoing);
            changes
                .after
                .entry(after_destination)
                .or_default()
                .push(incoming);
        }
        changes
    }

    fn new(result: &AnalyzeResult) -> Self {
        let mut dataset_roots = BTreeSet::new();
        let mut package_roots = BTreeSet::new();
        collect_roots(&result.current.tree, &mut dataset_roots, &mut package_roots);
        let roots = if dataset_roots.is_empty() {
            package_roots
        } else {
            dataset_roots
        };
        let heading = match roots.iter().collect::<Vec<_>>().as_slice() {
            [only] => (*only).clone(),
            _ => ".".to_owned(),
        };
        Self {
            heading,
            relocations: BTreeMap::new(),
            before: BTreeMap::new(),
            after: BTreeMap::new(),
        }
    }

    fn add_file(&mut self, path: &str, destination_folder: &str) {
        let after = destination(path, destination_folder);
        if path != after {
            self.before.entry(path.to_owned()).or_default();
            self.after.entry(after.clone()).or_default();
            self.relocations.insert(path.to_owned(), after);
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
            lines.push(format!("    {}/", self.heading));
            tree.draw("    ", &mut lines);
        }
        lines.push("  * File moved or its symbol contents changed.".to_owned());
        lines.push(
            "  Unchanged branches and symbols omitted; excluded files are not shown.".to_owned(),
        );
        lines
    }
}

/// Where a file lands when moved into `folder`; `.` or an empty folder is the
/// repository root, so the file keeps only its own name.
pub(super) fn destination(path: &str, folder: &str) -> String {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    if folder.is_empty() || folder == "." {
        leaf.to_owned()
    } else {
        format!("{folder}/{leaf}")
    }
}

fn collect_roots(
    node: &ContainerNode,
    dataset_roots: &mut BTreeSet<String>,
    package_roots: &mut BTreeSet<String>,
) {
    match node.level {
        Level::PackageGroup => {
            dataset_roots.insert(node.name.clone());
        }
        Level::Package => {
            package_roots.insert(node.name.clone());
        }
        _ => {}
    }
    for child in node.children.iter().flatten() {
        collect_roots(child, dataset_roots, package_roots);
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
