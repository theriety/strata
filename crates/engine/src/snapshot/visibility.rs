//! Scope-ladder verdicts (ADR-24, ADR-25): resolves adapter-stated visibility
//! scopes onto the final laminar container tree and decides, per declaration,
//! whether a strictly narrower spellable rung holds every user.

mod project;
mod rung;

use std::collections::{BTreeSet, HashMap, HashSet};

use smol_str::SmolStr;
use strata_core::visibility::exempt_from_advice;
use strata_ir::{
    AdvisoryUse, ContainerId, ContainerTree, Edge, EdgeKind, Exposure, Node, NodeId, ScopeLadder,
    ScopeLevel, VisibilityScope,
};

use self::project::scope_level;
use self::rung::needed_index;
use super::reexport::{name_lookup, re_export_targets, resolve_re_export};

/// Everything a verdict reads besides the declaration itself.
struct Facts<'a> {
    tree: &'a ContainerTree,
    files: &'a HashMap<SmolStr, ContainerId>,
    path_of: HashMap<ContainerId, &'a SmolStr>,
    /// File containers of each declaration's users, after re-export flattening.
    consumers: HashMap<NodeId, HashSet<ContainerId>>,
    /// The single scope statement of each declaration; two or more are unusable.
    scopes: HashMap<NodeId, Vec<&'a VisibilityScope>>,
    /// Signature and header exposures, keyed by the exposed declaration.
    exposures: HashMap<NodeId, Vec<&'a Exposure>>,
    /// Container each declaration lives in.
    containers: HashMap<NodeId, ContainerId>,
}

/// Projects visibility sidecars onto the final laminar tree and returns the
/// ladder verdict of every declaration whose adapter stated a scope.
pub(super) fn apply_visibility_scopes(
    nodes: &mut [Node],
    tree: &ContainerTree,
    files: &HashMap<SmolStr, ContainerId>,
    edges: &[Edge],
    visibility_scopes: &[VisibilityScope],
    exposures: &[Exposure],
    advisory_uses: &[AdvisoryUse],
) -> Vec<ScopeLadder> {
    let containers: HashMap<NodeId, ContainerId> =
        nodes.iter().map(|node| (node.id, node.container)).collect();
    // consumers are read the way `derive_visibility` reads them, after
    // re-export flattening: a consumer of a re-export is one of its original.
    let re_export_target = re_export_targets(edges);
    let name_of = name_lookup(nodes);
    let mut consumers: HashMap<NodeId, HashSet<ContainerId>> = HashMap::new();
    for edge in edges {
        if let Some(container) = containers.get(&edge.source) {
            let target = if edge.kind == EdgeKind::ReExport {
                edge.target
            } else {
                resolve_re_export(edge.target, &re_export_target, &name_of).unwrap_or(edge.target)
            };
            consumers.entry(target).or_default().insert(*container);
        }
    }
    // name-matched uses of code the loader cannot see: production users for
    // this verdict only, never edges, so nothing but advice reads them.
    for advisory in advisory_uses {
        if let Some(container) = containers.get(&advisory.user) {
            consumers
                .entry(advisory.used)
                .or_default()
                .insert(*container);
        }
    }
    let mut scopes: HashMap<NodeId, Vec<&VisibilityScope>> = HashMap::new();
    for scope in visibility_scopes {
        scopes.entry(scope.node).or_default().push(scope);
    }
    let mut by_exposed: HashMap<NodeId, Vec<&Exposure>> = HashMap::new();
    for exposure in exposures {
        by_exposed
            .entry(exposure.exposed)
            .or_default()
            .push(exposure);
    }
    let facts = Facts {
        tree,
        files,
        path_of: files.iter().map(|(path, id)| (*id, path)).collect(),
        consumers,
        scopes,
        exposures: by_exposed,
        containers,
    };

    let mut ladders = Vec::new();
    for node in nodes {
        // a re-export publishes its original; only the original is judged.
        if re_export_target.contains_key(&node.id) {
            continue;
        }
        let Some([scope]) = facts.scopes.get(&node.id).map(Vec::as_slice) else {
            if facts.scopes.contains_key(&node.id) {
                ladders.push(silent(node.id));
            }
            continue;
        };
        let Some(owner) = tree.containers().get(node.container.0 as usize) else {
            continue;
        };
        if owner.id != node.container || owner.level != ScopeLevel::File {
            continue;
        }
        // only a restricted declaration restates its declared level.
        if !scope.files.is_empty() {
            let Some(level) = scope_level(
                tree,
                files,
                node.container,
                &scope.files,
                scope.definition_file.as_ref(),
            ) else {
                ladders.push(silent(node.id));
                continue;
            };
            node.visibility = level;
        }
        // the finding path exempts these too; a stated, silent verdict keeps
        // the two from ever disagreeing about a public-API declaration.
        if exempt_from_advice(node) {
            ladders.push(silent(node.id));
            continue;
        }
        ladders.push(verdict(&facts, node, scope));
    }
    ladders
}

/// A ladder entry that never advises: the scope was stated but cannot be judged.
fn silent(node: NodeId) -> ScopeLadder {
    ScopeLadder {
        node,
        declared: None,
        needed: None,
        spelling: None,
    }
}

/// Compares the declared rung with the narrowest rung that holds every user.
fn verdict(facts: &Facts<'_>, node: &Node, scope: &VisibilityScope) -> ScopeLadder {
    let compared = compare(facts, node, scope);
    let Some((declared, needed, spelling)) = compared else {
        return silent(node.id);
    };
    ScopeLadder {
        node: node.id,
        declared: Some(declared),
        needed,
        spelling,
    }
}

/// Returns the declared level and, when strictly narrower, the needed one.
fn compare(
    facts: &Facts<'_>,
    node: &Node,
    scope: &VisibilityScope,
) -> Option<(ScopeLevel, Option<ScopeLevel>, Option<SmolStr>)> {
    let declared_index = usize::try_from(scope.declared_rung?).ok()?;
    let declared_rung = scope.expressible.get(declared_index)?;
    let level_of = |rung: &strata_ir::ScopeRung| {
        scope_level(
            facts.tree,
            facts.files,
            node.container,
            &rung.files,
            rung.definition_file.as_ref(),
        )
    };
    let declared = level_of(declared_rung)?;

    let footprint = footprint(facts, node)?;
    let Some(needed_index) = needed_index(&scope.expressible, &footprint) else {
        return Some((declared, None, None));
    };
    if needed_index >= declared_index {
        return Some((declared, None, None));
    }
    let needed_rung = scope.expressible.get(needed_index)?;
    let needed = level_of(needed_rung)?;
    Some((declared, Some(needed), needed_rung.spelling.clone()))
}

/// Every file that uses the declaration: its home, its consumers, and the files
/// of any exposer's declared reach. `None` when a user cannot be placed.
fn footprint<'a>(facts: &Facts<'a>, node: &Node) -> Option<BTreeSet<&'a SmolStr>> {
    let mut footprint = BTreeSet::new();
    footprint.insert(*facts.path_of.get(&node.container)?);
    for consumer in facts.consumers.get(&node.id).into_iter().flatten() {
        footprint.insert(*facts.path_of.get(consumer)?);
    }
    for exposure in facts.exposures.get(&node.id).into_iter().flatten() {
        let exposer_container = facts.containers.get(&exposure.exposer)?;
        footprint.insert(*facts.path_of.get(exposer_container)?);
        if !exposure.reach {
            continue;
        }
        // an exposer without one resolved declared rung (a trait impl, whose
        // reach is the narrower of trait and type) leaves the reach unknown.
        let [exposer] = facts.scopes.get(&exposure.exposer)?.as_slice() else {
            return None;
        };
        let rung = exposer
            .expressible
            .get(usize::try_from(exposer.declared_rung?).ok()?)?;
        footprint.extend(rung.files.iter());
    }
    Some(footprint)
}
