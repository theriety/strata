//! The container arena and the qualification of elected sibling names.

use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use strata_ir::{Container, ContainerId, ScopeLevel};

use crate::analyze::scoring::ContainerSpec;

/// Interns candidate containers with dense ids, keeping the sibling names taken
/// under each `(parent, level)` scope as a last-resort collision guard.
#[derive(Default)]
pub(in crate::analyze) struct ContainerArena {
    /// The containers interned so far, indexed by their dense id.
    pub(in crate::analyze) containers: Vec<Container>,
    /// The sibling names already taken under each `(parent, level)` scope.
    used: BTreeMap<(Option<u32>, ScopeLevel), BTreeSet<SmolStr>>,
}

impl ContainerArena {
    /// Interns `spec` under the next dense id and returns it.
    ///
    /// Every reachable naming path is injective by construction — folder names
    /// through `qualify_folder_names`, elected group/package/domain names
    /// through `qualify_elected` — so the numeric suffix below is a backstop
    /// for names that textually embed another sibling's qualifier: a real
    /// directory literally named like `http (ai)` colliding with a qualified
    /// twin (non-adversarial), or a hand-built snapshot reusing a qualified
    /// shape outright (adversarial). Neither arises from real elected paths.
    pub(in crate::analyze) fn push(&mut self, spec: ContainerSpec<'_>) -> ContainerId {
        let ContainerSpec {
            name,
            level,
            parent,
            synthetic,
        } = spec;
        let siblings = self
            .used
            .entry((parent.map(|parent| parent.0), level))
            .or_default();
        let mut unique = name.clone();
        let mut suffix = 2_u32;
        while siblings.contains(&unique) {
            unique = SmolStr::new(format!("{name}-{suffix}"));
            suffix = suffix.saturating_add(1);
        }
        siblings.insert(unique.clone());
        let id = ContainerId(u32::try_from(self.containers.len()).unwrap_or(u32::MAX));
        self.containers.push(Container {
            id,
            name: unique,
            level,
            parent,
            synthetic,
        });
        id
    }
}

/// Keeps the lexicographically smallest anchor folder name seen per cluster.
pub(in crate::analyze) fn anchor_min(
    anchors: &mut BTreeMap<u32, SmolStr>,
    cluster: u32,
    name: &SmolStr,
) {
    anchors
        .entry(cluster)
        .and_modify(|held| {
            if *name < *held {
                *held = name.clone();
            }
        })
        .or_insert_with(|| name.clone());
}

/// Qualifies elected sibling names into injective ones: a raw name shared by
/// two clusters under one parent gains each cluster's dot-encoded anchor
/// folder — `app (pa.app.x)` — the same real-location style folder twins use,
/// so no reachable elected path ever needs the arena's numeric backstop.
/// Records `id`'s undecorated elected key when `qualify_elected` decorated its
/// display `name`, so the render boundary can strip a folder's increment against
/// the real key rather than the anchor-decorated label. An undecorated name (the
/// common, no-collision case) already matches the folder-key prefix, so it is
/// left out — keeping the map empty and the render byte-identical to before.
pub(in crate::analyze) fn record_undecorated_key(
    key_by_id: &mut BTreeMap<u32, SmolStr>,
    id: ContainerId,
    raw: &SmolStr,
    display: &SmolStr,
) {
    if raw != display {
        key_by_id.insert(id.0, raw.clone());
    }
}

pub(in crate::analyze) fn qualify_elected(
    raw: &BTreeMap<u32, (u32, SmolStr)>,
    anchors: &BTreeMap<u32, SmolStr>,
) -> BTreeMap<u32, SmolStr> {
    let mut sibling_count: BTreeMap<(u32, &SmolStr), u32> = BTreeMap::new();
    for (parent, name) in raw.values() {
        *sibling_count.entry((*parent, name)).or_default() += 1;
    }
    raw.iter()
        .map(|(&cluster, (parent, name))| {
            let colliding = sibling_count.get(&(*parent, name)).copied().unwrap_or(0) > 1;
            let name = if colliding {
                let anchor = anchors
                    .get(&cluster)
                    .cloned()
                    .unwrap_or_else(|| SmolStr::new("workspace"));
                SmolStr::new(format!("{name} ({})", anchor.replace('/', ".")))
            } else {
                name.clone()
            };
            (cluster, name)
        })
        .collect()
}
