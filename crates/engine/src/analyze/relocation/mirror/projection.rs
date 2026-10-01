//! Folder and path projection for candidate files, in repository-relative and
//! rooted coordinates.

use smol_str::SmolStr;
use strata_core::cluster::Partition;

use crate::analyze::relocation::PipelineSolver;
use crate::analyze::relocation::mirror::{PolishEvidence, ProjectedFolder};
use crate::narrate::{normalize_physical_path, project_physical_path};

impl PipelineSolver<'_> {
    pub(in crate::analyze::relocation) fn projected_folder_for_vertex(
        &self,
        parts: &Partition,
        vertex: u32,
        evidence: &PolishEvidence,
    ) -> Option<ProjectedFolder> {
        let scc = self.condensation.membership.get(vertex as usize)?.0;
        if let Some(intended) = evidence.applied_destination(scc) {
            return Some(ProjectedFolder::Rooted(SmolStr::new(intended)));
        }
        let cluster = parts.cluster_of(scc)?;
        if let Some(home) = self.retained_cycle_home(parts, vertex, evidence) {
            return Some(ProjectedFolder::RepositoryRelative(home.clone()));
        }
        self.real_folder_names
            .get(cluster.0 as usize)
            .cloned()
            .map(ProjectedFolder::RepositoryRelative)
    }

    /// Original physical home only while the SCC has not moved or mirrored.
    pub(in crate::analyze::relocation) fn retained_cycle_home(
        &self,
        parts: &Partition,
        vertex: u32,
        evidence: &PolishEvidence,
    ) -> Option<&SmolStr> {
        let scc = self.condensation.membership.get(vertex as usize)?.0;
        let cluster = parts.cluster_of(scc)?;
        if evidence.applied_destination(scc).is_some()
            || self.pass_start_partition.cluster_of(scc) != Some(cluster)
        {
            return None;
        }
        self.cycle_home_by_vertex.get(&vertex)
    }

    pub(in crate::analyze::relocation) fn repository_path(&self, path: &str) -> String {
        let relative: Vec<String> = path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect();
        project_physical_path(self.root_name.as_str(), &relative).join("/")
    }

    pub(super) fn candidate_path(&self, path: &str) -> String {
        let path: Vec<String> = path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect();
        normalize_physical_path(self.root_name.as_str(), &path).join("/")
    }
}
