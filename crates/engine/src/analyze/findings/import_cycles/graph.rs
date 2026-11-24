//! The file-import graph: files in path order, one edge per importing pair.

use std::collections::{BTreeMap, BTreeSet};

use unlace_ir::{FileImport, ImportKind};

/// The directed graph of import facts over files.
///
/// Files are numbered in ascending path order, so every tie the cut breaks on
/// an index breaks on path order. Each edge keeps every statement behind it.
pub(super) struct ImportGraph<'a> {
    /// The files that import or are imported, ascending.
    pub(super) files: Vec<&'a str>,
    /// The statements behind each `(importer, imported)` edge.
    edges: BTreeMap<(usize, usize), Vec<&'a FileImport>>,
}

impl<'a> ImportGraph<'a> {
    /// Builds the graph from the IR's import facts, leaving out self-imports
    /// and module declarations (ADR-35).
    ///
    /// A [`ImportKind::ModuleDeclaration`] (Rust `mod child;`) states
    /// ownership, not a use, so it never forms or closes a cycle. A pair whose
    /// facts are all declarations has no edge; a pair that also states a
    /// `Value` or `SideEffect` use keeps an edge with just those statements.
    pub(super) fn new(imports: &'a [FileImport]) -> Self {
        let kept: Vec<&FileImport> = imports
            .iter()
            .filter(|import| import.importer != import.imported)
            .filter(|import| import.kind != ImportKind::ModuleDeclaration)
            .collect();
        let files: Vec<&str> = kept
            .iter()
            .flat_map(|import| [import.importer.as_str(), import.imported.as_str()])
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let index: BTreeMap<&str, usize> = files
            .iter()
            .enumerate()
            .map(|(position, file)| (*file, position))
            .collect();
        let mut edges: BTreeMap<(usize, usize), Vec<&FileImport>> = BTreeMap::new();
        for import in kept {
            let (Some(&importer), Some(&imported)) = (
                index.get(import.importer.as_str()),
                index.get(import.imported.as_str()),
            ) else {
                continue;
            };
            edges.entry((importer, imported)).or_default().push(import);
        }
        Self { files, edges }
    }

    /// The path of the file at `index`.
    pub(super) fn file(&self, index: usize) -> &'a str {
        self.files.get(index).copied().unwrap_or_default()
    }

    /// The index of `file`, when it imports or is imported.
    pub(super) fn index_of(&self, file: &str) -> Option<usize> {
        self.files.binary_search(&file).ok()
    }

    /// Whether `importer` imports `imported`.
    pub(super) fn has_edge(&self, importer: usize, imported: usize) -> bool {
        self.edges.contains_key(&(importer, imported))
    }

    /// The statements behind the edge `importer -> imported`.
    pub(super) fn statements(&self, importer: usize, imported: usize) -> &[&'a FileImport] {
        self.edges
            .get(&(importer, imported))
            .map_or(&[], Vec::as_slice)
    }

    /// Every edge, ascending.
    pub(super) fn edges(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.edges.keys().copied()
    }
}

/// The components of two or more files, each ascending, ordered by their
/// first file (Kosaraju; the passes walk files in path order).
pub(super) fn strongly_connected(graph: &ImportGraph<'_>) -> Vec<Vec<usize>> {
    let mut forward: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut backward: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (importer, imported) in graph.edges() {
        forward.entry(importer).or_default().push(imported);
        backward.entry(imported).or_default().push(importer);
    }
    let files = 0..graph.files.len();

    // first pass: files in order of DFS completion.
    let mut finished: Vec<usize> = Vec::new();
    let mut visited: BTreeSet<usize> = BTreeSet::new();
    for root in files {
        if !visited.insert(root) {
            continue;
        }
        let mut stack: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some((file, next)) = stack.last_mut() {
            let target = forward.get(file).and_then(|targets| targets.get(*next));
            if let Some(&target) = target {
                *next += 1;
                if visited.insert(target) {
                    stack.push((target, 0));
                }
            } else {
                finished.push(*file);
                stack.pop();
            }
        }
    }

    // second pass on the reversed graph, latest finisher first.
    let mut assigned: BTreeSet<usize> = BTreeSet::new();
    let mut components: Vec<Vec<usize>> = Vec::new();
    for &root in finished.iter().rev() {
        if !assigned.insert(root) {
            continue;
        }
        let mut component = vec![root];
        let mut stack = vec![root];
        while let Some(file) = stack.pop() {
            for &source in backward.get(&file).into_iter().flatten() {
                if assigned.insert(source) {
                    component.push(source);
                    stack.push(source);
                }
            }
        }
        if component.len() > 1 {
            component.sort_unstable();
            components.push(component);
        }
    }
    components.sort();
    components
}
