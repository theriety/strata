//! Solver construction: every seed-independent pipeline input, computed once.

use smol_str::SmolStr;
use strata_core::cluster::Partition;
use strata_core::condense::condense;
use strata_core::score::Coefficients;
use strata_ir::{ScopeLevel, Snapshot};

use crate::analyze::layout::{real_dir_partition, relieve_over_capacity, synthesize_roof_rebuild};
use crate::analyze::relocation::mirror::mirror_rules;
use crate::analyze::relocation::solver::{build_file_graph, test_zone_marks};
use crate::analyze::relocation::{
    PipelineSolver, RelocationIdentityGuard, TestPolicy, cycle_homes,
    file_facts_repository_relative, file_inventory,
};
use crate::analyze::scoring::{ProfileSource, level_caps};

impl<'a> PipelineSolver<'a> {
    /// Builds the solver, computing every seed-independent pipeline input once.
    #[allow(clippy::too_many_lines)]
    pub(in crate::analyze) fn new<P: ProfileSource + ?Sized>(
        snapshot: &'a Snapshot,
        profile_source: &P,
        coefficients: Coefficients,
        seed_identity: bool,
        tests: &'a TestPolicy,
    ) -> Self {
        let profile = profile_source.profile();
        let ir = snapshot.ir();
        let weights = profile.weights.kind_weights();
        let (files, index_of) = file_inventory(ir);

        let test_zone = test_zone_marks(tests, &files, &ir.nodes);

        let file_graph = build_file_graph(
            &ir.edges,
            &ir.nodes,
            &index_of,
            files.len(),
            &weights,
            &test_zone,
        );
        let condensation = condense(&file_graph);
        let forbidden_file_patterns: Vec<glob::Pattern> = profile
            .relocation
            .forbid_file_moves
            .iter()
            .filter_map(|pattern| glob::Pattern::new(pattern).ok())
            .collect();
        let forbidden_symbol_files = profile
            .relocation
            .forbid_symbol_moves
            .iter()
            .filter_map(|pattern| glob::Pattern::new(pattern).ok())
            .collect();
        let pinned_vertex: Vec<bool> = files
            .iter()
            .enumerate()
            .map(|(index, file)| {
                (profile.relocation.pin_detected_test_files
                    && test_zone.get(index).copied().unwrap_or(false))
                    || forbidden_file_patterns.iter().any(|pattern| {
                        pattern.matches_path(std::path::Path::new(file.name.as_str()))
                    })
            })
            .collect();
        let pinned_scc: Vec<bool> = condensation
            .members
            .iter()
            .map(|members| {
                members.iter().any(|member| {
                    pinned_vertex
                        .get(member.0 as usize)
                        .copied()
                        .unwrap_or(false)
                })
            })
            .collect();
        let caps = level_caps(&profile.capacity);
        let reverse_dag = condensation.dag.reversed();

        // folders are reality: the identity layout and the search's folder
        // partition start as the same object — each file SCC in its real
        // directory — so anchored seeding just clones it.
        let (identity_partition, identity_names, identity_synthetic) =
            real_dir_partition(&files, &condensation);
        let cycle_home_by_vertex = cycle_homes(&files, &condensation);
        let identity = seed_identity.then(|| identity_partition.clone());

        // FIX03 relieves over-capacity binding in the search grain itself: a
        // physical folder over its direct-file-plus-direct-child budget is
        // pre-split along priced connectivity into evidence-backed child groups
        // named to path-extend their base folder. The objective prices what this
        // creates, so every non-identity seed starts from a layout the split can
        // win from instead of only being able to shed files out of the over-cap
        // folder.
        let (relieved_files, mut search_partition, mut folder_names, mut folder_synthetic) =
            relieve_over_capacity(
                files,
                &condensation,
                &file_graph,
                &identity_partition,
                &identity_names,
                &identity_synthetic,
                caps.folder,
            );
        if pinned_scc.iter().any(|pinned| *pinned) {
            let mut assignment = search_partition.assignment().to_vec();
            for (scc, pinned) in pinned_scc.iter().copied().enumerate() {
                if pinned
                    && let (Some(slot), Some(home)) = (
                        assignment.get_mut(scc),
                        identity_partition.cluster_of(u32::try_from(scc).unwrap_or(u32::MAX)),
                    )
                {
                    *slot = home;
                }
            }
            search_partition =
                Partition::from_assignment(assignment, search_partition.cluster_count());
        }
        // FIX09 (naming-incoherence): a misnamed roof is invisible to edge-driven
        // search — the strangers under it carry no priced edge to pull them out,
        // so every seed converges on the same welded layout and no candidate can
        // ever propose the split. Where the signature fires, this synthesizes one
        // alternative start: zero-priced strangers regrouped by their own shared
        // tokens, a residual roof that misdescribes its remaining residents
        // dissolved into priced-connected places, each new place labeled from
        // member names to path-extend its base folder.
        // The proposal enters the ordinary pool at offset 1 — polish still runs,
        // the vetoes still bind, the objective still decides — and when it does
        // not fire, nothing downstream changes at all.
        let roof_rebuild = synthesize_roof_rebuild(
            &relieved_files,
            &condensation,
            &file_graph,
            &test_zone,
            &pinned_scc,
            &search_partition,
            &mut folder_names,
            &mut folder_synthetic,
        );
        let relocation_identity = RelocationIdentityGuard::new(
            &relieved_files,
            &condensation,
            &identity_partition,
            &search_partition,
            roof_rebuild.as_ref(),
        );
        let relocation_identity = if profile.relocation.allow_cross_package_moves {
            relocation_identity.lift_package_wall()
        } else {
            relocation_identity
        };
        let root_name = ir
            .containers
            .containers()
            .iter()
            .find(|container| container.level == ScopeLevel::PackageGroup)
            .map_or_else(|| SmolStr::new("workspace"), |group| group.name.clone());
        let facts = file_facts_repository_relative(
            snapshot,
            &weights,
            profile.capacity.folder,
            &relieved_files,
            &test_zone,
        );
        let real_is_identity = search_partition == identity_partition;

        Self {
            snapshot,
            test_zone,
            pinned_scc,
            forbidden_symbol_files,
            mirror_rules: mirror_rules(profile),
            mirror_enabled: profile.relocation.test_mirroring.enabled,
            pin_test_symbols: profile.relocation.pin_detected_test_symbols,
            files: relieved_files,
            index_of,
            condensation,
            relocation_identity,
            reverse_dag,
            caps,
            capacity: profile.capacity,
            coefficients,
            weights,
            same_file_symbol: profile.weights.same_file_symbol,
            same_file_type: profile.weights.same_file_type,
            identity,
            pass_start_partition: identity_partition,
            cycle_home_by_vertex,
            real_is_identity,
            real_partition: search_partition,
            real_folder_names: folder_names,
            real_folder_synthetic: folder_synthetic,
            base_seed: profile.seed,
            root_name,
            facts,
            roof_rebuild,
        }
    }
}
